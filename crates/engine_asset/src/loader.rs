//! Background asset loading: runs import work off the main thread,
//! without blocking the frame loop that polls for results.
//!
//! No actual importers yet (glTF/texture/audio are the next iterations)
//! — this is the generic "run this future in the background, check on it
//! each frame without blocking" machinery they'll be built on.

use std::future::Future;

use tokio::runtime::Runtime;
use tokio::sync::oneshot;

use crate::error::AssetError;

/// Owns the background tokio runtime that [`AssetLoader::spawn`] runs
/// loading work on.
///
/// One of these is enough for a whole application — cheap to share (wrap
/// in an `Arc` if multiple owners are ever needed; not yet required since
/// nothing but this crate constructs one).
pub struct AssetLoader {
    runtime: Runtime,
}

impl AssetLoader {
    /// Starts a new background loading runtime.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError::RuntimeInit`] if the OS refuses to start the
    /// runtime's worker threads (e.g. thread/resource exhaustion).
    pub fn new() -> Result<Self, AssetError> {
        Runtime::new()
            .map(|runtime| Self { runtime })
            .map_err(|err| AssetError::RuntimeInit(err.to_string()))
    }

    /// Spawns `future` onto the background runtime and returns a
    /// [`LoadHandle`] for polling its result without blocking.
    ///
    /// `future` runs to completion even if the returned [`LoadHandle`] is
    /// dropped early (e.g. the caller stopped caring about the result) —
    /// it isn't cancelled, just its result goes unread.
    pub fn spawn<T, F>(&self, future: F) -> LoadHandle<T>
    where
        F: Future<Output = Result<T, AssetError>> + Send + 'static,
        T: Send + 'static,
    {
        let (sender, receiver) = oneshot::channel();
        self.runtime.spawn(async move {
            let result = future.await;
            // Ignore a closed receiver: the caller dropped the
            // `LoadHandle` and no longer wants the result, which isn't an
            // error on the producing side.
            let _ = sender.send(result);
        });
        LoadHandle { receiver }
    }
}

/// The current state of a background load, as observed by
/// [`LoadHandle::poll`].
#[derive(Debug)]
pub enum LoadStatus<T> {
    /// Still running.
    Pending,
    /// Finished successfully.
    Ready(T),
    /// Finished with an error, or was dropped before finishing.
    Failed(AssetError),
}

/// A handle to a background load spawned via [`AssetLoader::spawn`].
///
/// Call [`LoadHandle::poll`] once per frame (or whenever convenient) from
/// ordinary synchronous code — no `async`/`.await` needed on the caller's
/// side, and it never blocks.
pub struct LoadHandle<T> {
    receiver: oneshot::Receiver<Result<T, AssetError>>,
}

impl<T> LoadHandle<T> {
    /// Checks whether the load has finished, without blocking.
    ///
    /// Once this returns [`LoadStatus::Ready`] or [`LoadStatus::Failed`],
    /// the result has been consumed — later calls return
    /// [`LoadStatus::Failed`]`(`[`AssetError::LoadTaskDropped`]`)`, since
    /// the underlying channel only delivers its value once.
    pub fn poll(&mut self) -> LoadStatus<T> {
        match self.receiver.try_recv() {
            Ok(Ok(value)) => LoadStatus::Ready(value),
            Ok(Err(err)) => LoadStatus::Failed(err),
            Err(oneshot::error::TryRecvError::Empty) => LoadStatus::Pending,
            Err(oneshot::error::TryRecvError::Closed) => {
                LoadStatus::Failed(AssetError::LoadTaskDropped)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Polls `handle` until it stops being `Pending`, or panics after a
    /// generous timeout — background task scheduling isn't instantaneous,
    /// but it's not supposed to hang either.
    fn poll_until_settled<T>(handle: &mut LoadHandle<T>) -> LoadStatus<T> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match handle.poll() {
                LoadStatus::Pending => {
                    assert!(
                        Instant::now() < deadline,
                        "load never settled within timeout"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                settled => return settled,
            }
        }
    }

    #[test]
    fn spawn_delivers_success_value() {
        let loader = AssetLoader::new().unwrap();
        let mut handle = loader.spawn(async { Ok::<_, AssetError>(42) });

        match poll_until_settled(&mut handle) {
            LoadStatus::Ready(value) => assert_eq!(value, 42),
            other => panic!("expected Ready(42), got {other:?}"),
        }
    }

    #[test]
    fn spawn_delivers_error() {
        let loader = AssetLoader::new().unwrap();
        let mut handle = loader.spawn(async { Err::<u32, _>(AssetError::LoadTaskDropped) });

        match poll_until_settled(&mut handle) {
            LoadStatus::Failed(AssetError::LoadTaskDropped) => {}
            other => panic!("expected Failed(LoadTaskDropped), got {other:?}"),
        }
    }

    #[test]
    fn poll_is_pending_until_explicitly_unblocked() {
        let loader = AssetLoader::new().unwrap();
        let (unblock_tx, unblock_rx) = oneshot::channel::<()>();

        let mut handle = loader.spawn(async move {
            // Cannot complete until `unblock_tx` fires below, so the
            // first `poll()` is guaranteed `Pending`, not just "probably
            // still running".
            unblock_rx.await.ok();
            Ok::<_, AssetError>(7)
        });

        assert!(matches!(handle.poll(), LoadStatus::Pending));

        unblock_tx.send(()).unwrap();
        match poll_until_settled(&mut handle) {
            LoadStatus::Ready(value) => assert_eq!(value, 7),
            other => panic!("expected Ready(7), got {other:?}"),
        }
    }

    #[test]
    fn does_not_block_the_calling_thread() {
        let loader = AssetLoader::new().unwrap();
        let mut handle = loader.spawn(async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok::<_, AssetError>(())
        });

        let started = Instant::now();
        let status = handle.poll();
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "poll() must return immediately, not wait for the 200ms sleep"
        );
        assert!(matches!(status, LoadStatus::Pending));
    }

    #[test]
    fn dropping_handle_does_not_panic_the_background_task() {
        let loader = AssetLoader::new().unwrap();
        let handle = loader.spawn(async { Ok::<_, AssetError>(1) });
        drop(handle);
        // Nothing to assert beyond "didn't panic"; the spawned task's
        // `sender.send(..)` on a closed receiver is handled (ignored).
    }
}
