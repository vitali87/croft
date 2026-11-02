use std::thread::{Builder, JoinHandle};

use anyhow::{Context, Result};
use tokio::runtime::{Builder as RtBuilder, Handle};
use tokio::sync::oneshot;

pub struct LspRuntime {
    handle: Handle,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl LspRuntime {
    pub fn new() -> Result<Self> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        // Built here rather than on the croft-lsp thread, which used to hand
        // the handle back over a channel: the editor then waited for a brand
        // new thread to be scheduled, milliseconds on a launch whose other
        // threads already filled every core, before its first frame. The
        // workers start on their own threads either way.
        let rt = RtBuilder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("croft-lsp-worker")
            .build()
            .context("build croft-lsp tokio runtime")?;
        let handle = rt.handle().clone();
        let thread = Builder::new()
            .name("croft-lsp".into())
            .spawn(move || {
                rt.block_on(async {
                    let _ = shutdown_rx.await;
                });
            })
            .context("spawn croft-lsp thread")?;

        Ok(Self {
            handle,
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        })
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }
}

impl Drop for LspRuntime {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn runtime_starts_and_drops_cleanly() {
        let rt = LspRuntime::new().expect("runtime");
        let handle = rt.handle().clone();
        let value = handle.block_on(async { 21 + 21 });
        assert_eq!(value, 42);
        drop(rt);
    }

    #[test]
    fn runtime_supports_spawn() {
        let rt = LspRuntime::new().expect("runtime");
        let join = rt.handle().spawn(async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            7
        });
        let v = rt.handle().block_on(async { join.await.unwrap() });
        assert_eq!(v, 7);
    }
}
