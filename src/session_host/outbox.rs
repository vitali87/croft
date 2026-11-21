//! A client's outbound queue: queueing frames without ever blocking on the socket,
//! counting what was delivered, closing once drained, and the writer thread that drains
//! it.

use super::*;

/// How many bytes of undelivered output one client may accumulate before it
/// is declared too slow to keep up. The PTY pump must never wait on a
/// socket (#228), so a peer that stops draining cannot be allowed to apply
/// backpressure — the only alternatives are dropping it or growing the
/// queue without bound. A wedged client's queue fills at the rate the inner
/// croft produces output, so this is a time bound in disguise: roughly a
/// full screen of dense repaints at a large window size, which a healthy
/// peer drains in milliseconds and a dead one never drains at all.
pub(super) const CLIENT_QUEUE_LIMIT: usize = 4 * 1024 * 1024;

impl Outbox {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(OutboxState::default()),
            wake: std::sync::Condvar::new(),
        })
    }

    /// Queue one frame for delivery. Returns false once the peer is dead —
    /// either already marked, or newly over the queue limit. Never blocks
    /// on the socket, so this is safe to call from the PTY pump.
    pub(super) fn push(&self, frame: &[u8]) -> bool {
        self.push_seq(frame).is_some()
    }

    /// [`push`](Self::push), reporting the frame's number in this outbox's
    /// delivery order so a caller can wait for that exact frame to reach the
    /// peer ([`delivered_through`](Self::delivered_through)). `None` means
    /// the peer is dead and nothing more will be delivered.
    pub(super) fn push_seq(&self, frame: &[u8]) -> Option<u64> {
        let mut state = self.queue.lock().unwrap();
        if state.dead {
            return None;
        }
        // Overflow is judged against the BACKLOG the new frame joins, not
        // the total including it, so one frame larger than the whole limit
        // still reaches a healthy peer rather than killing it for a single
        // big repaint (a Kitty image, a full-screen redraw at a huge size).
        // What marks a peer dead is a backlog that was already at the limit
        // before this frame arrived — that only happens when it has stopped
        // draining.
        let backlog = state.bytes;
        state.bytes += frame.len();
        state.pending.push_back(frame.to_vec());
        state.queued += 1;
        let seq = state.queued;
        if backlog > CLIENT_QUEUE_LIMIT {
            state.dead = true;
            state.pending.clear();
            state.bytes = 0;
            self.wake.notify_all();
            return None;
        }
        self.wake.notify_all();
        Some(seq)
    }

    /// Count one frame as actually written to the socket. Called by the
    /// writer thread, which is the only place delivery is observable.
    pub(super) fn mark_delivered(&self) {
        self.queue.lock().unwrap().delivered += 1;
    }

    /// Has the frame numbered `seq` reached the peer? A dead outbox answers
    /// true: it will never deliver anything, so a barrier waiting on it must
    /// stop waiting rather than hold everyone else up.
    pub(super) fn delivered_through(&self, seq: u64) -> bool {
        let state = self.queue.lock().unwrap();
        state.dead || state.delivered >= seq
    }

    /// Deliver what is already queued, then end the connection: the writer
    /// drains the backlog and exits rather than parking on an empty queue.
    ///
    /// [`kill`](Self::kill) is the wrong tool for a goodbye frame - it
    /// clears `pending`, so the frame that says goodbye would never reach
    /// the peer. Used by the swap refusal (#321), whose whole point is that
    /// the client READS the HostSwap frame before the socket closes.
    pub(super) fn close_when_drained(&self) {
        let mut state = self.queue.lock().unwrap();
        state.closing = true;
        self.wake.notify_all();
    }

    /// Mark the peer gone and wake its writer so the thread can exit.
    pub(super) fn kill(&self) {
        let mut state = self.queue.lock().unwrap();
        state.dead = true;
        state.pending.clear();
        state.bytes = 0;
        self.wake.notify_all();
    }

    #[cfg(test)]
    pub(super) fn is_dead(&self) -> bool {
        self.queue.lock().unwrap().dead
    }

    /// Block until a frame is available or the peer is dead. `None` means
    /// the writer thread should exit.
    pub(super) fn pop_blocking(&self) -> Option<Vec<u8>> {
        let mut state = self.queue.lock().unwrap();
        loop {
            if state.dead {
                return None;
            }
            if let Some(frame) = state.pending.pop_front() {
                state.bytes -= frame.len();
                return Some(frame);
            }
            // Drained and closing: the goodbye frame is on the wire, so the
            // writer's work is done.
            if state.closing {
                return None;
            }
            state = self.wake.wait(state).unwrap();
        }
    }
}

/// Shut a client's socket down WITHOUT taking its stream lock, given the
/// raw fd captured at registration ([`Client::fd`]).
///
/// The lock is held by that client's writer thread for as long as a frame
/// write takes — up to [`WRITE_FRAME_DEADLINE`] against a wedged peer. Any
/// disconnect path that waited for it would inherit exactly the multi-second
/// stall the outbox design exists to remove (#228), and would do so on the
/// attach path, where a reconnecting client evicts its ghost (#229).
///
/// `shutdown(2)` is safe to call on a fd another thread is mid-write on, and
/// is precisely what unblocks that writer. The fd stays valid because the
/// `Client` holds an `Arc<Mutex<UnixStream>>` that owns it; worst case the
/// writer's in-flight `write` fails, which is the intent.
pub(super) fn shutdown_now(fd: std::os::fd::RawFd) {
    unsafe { libc::shutdown(fd, libc::SHUT_RDWR) };
}

/// Drain `outbox` into `stream` until the peer dies. One of these runs per
/// client, so a slow socket stalls only its own thread.
pub(super) fn spawn_writer(
    stream: Arc<Mutex<UnixStream>>,
    outbox: Arc<Outbox>,
    fd: std::os::fd::RawFd,
) {
    std::thread::spawn(move || {
        while let Some(frame) = outbox.pop_blocking() {
            // The deadline still applies per frame: it bounds how long this
            // thread lingers on a wedged peer before releasing the socket,
            // and no other client's output waits behind it.
            let ok = write_frame_bounded(
                &mut stream.lock().unwrap(),
                &frame,
                Instant::now() + WRITE_FRAME_DEADLINE,
            );
            if !ok {
                outbox.kill();
                break;
            }
            outbox.mark_delivered();
        }
        // Unblock the client thread's read so it deregisters promptly
        // instead of lingering until the peer happens to send something.
        shutdown_now(fd);
    });
}
