use std::sync::mpsc;
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
        let (handle_tx, handle_rx) = mpsc::sync_channel::<Handle>(1);
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let thread = Builder::new()
            .name("croft-lsp".into())
            .spawn(move || {
                let rt = RtBuilder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .thread_name("croft-lsp-worker")
                    .build()
                    .expect("build croft-lsp tokio runtime");
                let _ = handle_tx.send(rt.handle().clone());
                rt.block_on(async {
                    let _ = shutdown_rx.await;
                });
            })
            .context("spawn croft-lsp thread")?;

        let handle = handle_rx
            .recv()
            .context("receive croft-lsp runtime handle")?;

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
