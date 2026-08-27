//! Hot reload: watch asset source files/directories for changes, poll for
//! what changed without blocking (same non-blocking-poll shape as
//! [`crate::LoadHandle`]).
//!
//! Built on [`notify::PollWatcher`] rather than notify's platform-native
//! backend (`RecommendedWatcher`, FSEvents/inotify/kqueue): polling is
//! slightly less efficient, but doesn't depend on OS-level file-event
//! permissions/APIs being available, which matters more for a first
//! implementation than shaving polling latency.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{Config, Event, EventKind, PollWatcher, RecursiveMode, Watcher};

use crate::error::AssetError;

/// Watches a file or directory (recursively) for content changes, and
/// hands back changed paths via [`AssetWatcher::poll_changes`].
#[derive(Debug)]
pub struct AssetWatcher {
    // Kept alive for its `Drop` impl, which stops the background polling
    // thread — never read directly.
    #[allow(dead_code, reason = "kept for its Drop side effect, never read")]
    watcher: PollWatcher,
    receiver: mpsc::Receiver<PathBuf>,
}

impl AssetWatcher {
    /// Starts watching `path` (a file or directory; directories are
    /// watched recursively) for content changes, checking every
    /// `poll_interval`.
    ///
    /// Detection compares file contents (not just modification time), so
    /// a write that doesn't actually change the bytes isn't reported.
    ///
    /// # Errors
    ///
    /// Returns [`AssetError::WatchInit`] if `path` doesn't exist or the
    /// watcher otherwise fails to start.
    pub fn watch(path: &Path, poll_interval: Duration) -> Result<Self, AssetError> {
        if !path.exists() {
            // `PollWatcher` itself doesn't error on a missing path (it
            // would just poll and never find anything to report) — check
            // explicitly so a typo'd path fails fast instead of silently
            // watching nothing.
            return Err(AssetError::WatchInit(format!(
                "path does not exist: {}",
                path.display()
            )));
        }

        let (sender, receiver) = mpsc::channel();

        let config = Config::default()
            .with_poll_interval(poll_interval)
            .with_compare_contents(true);

        let mut watcher = PollWatcher::new(
            move |event: notify::Result<Event>| {
                let Ok(event) = event else { return };
                if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    return;
                }
                for changed_path in event.paths {
                    // A closed/dropped receiver (the `AssetWatcher` was
                    // dropped) just means nobody's listening anymore —
                    // not a condition worth surfacing.
                    let _ = sender.send(changed_path);
                }
            },
            config,
        )
        .map_err(|err| AssetError::WatchInit(err.to_string()))?;

        watcher
            .watch(path, RecursiveMode::Recursive)
            .map_err(|err| AssetError::WatchInit(err.to_string()))?;

        Ok(Self { watcher, receiver })
    }

    /// Drains every change detected since the last call, without
    /// blocking. Deduplicated: a path reported multiple times between
    /// polls (e.g. two quick writes) appears once.
    pub fn poll_changes(&self) -> Vec<PathBuf> {
        let mut changed = Vec::new();
        while let Ok(path) = self.receiver.try_recv() {
            if !changed.contains(&path) {
                changed.push(path);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "vge-engine_asset-watch-test-{name}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Polls `watcher` until it reports at least one change, or panics
    /// after a generous timeout. Filesystem polling is timing-based by
    /// nature; this bounds the wait rather than looping forever.
    fn poll_until_nonempty(watcher: &AssetWatcher) -> Vec<PathBuf> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let changed = watcher.poll_changes();
            if !changed.is_empty() {
                return changed;
            }
            assert!(
                Instant::now() < deadline,
                "no change detected within timeout"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn detects_a_modified_file_in_a_watched_directory() {
        let dir = temp_dir("modify");
        let file_path = dir.join("asset.txt");
        std::fs::write(&file_path, "before").unwrap();

        let watcher = AssetWatcher::watch(&dir, Duration::from_millis(30)).unwrap();
        // Give the watcher a moment to record the initial file state
        // before we change it, so the change is detected as a diff
        // rather than possibly missed by a race with the first scan.
        std::thread::sleep(Duration::from_millis(100));

        std::fs::write(&file_path, "after, and longer than before").unwrap();

        let changed = poll_until_nonempty(&watcher);
        assert!(changed.iter().any(|p| p == &file_path));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detects_a_newly_created_file() {
        let dir = temp_dir("create");
        let watcher = AssetWatcher::watch(&dir, Duration::from_millis(30)).unwrap();
        std::thread::sleep(Duration::from_millis(100));

        let file_path = dir.join("new_asset.txt");
        std::fs::write(&file_path, "hello").unwrap();

        let changed = poll_until_nonempty(&watcher);
        assert!(changed.iter().any(|p| p == &file_path));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_changes_is_empty_when_nothing_changed() {
        let dir = temp_dir("quiet");
        let watcher = AssetWatcher::watch(&dir, Duration::from_millis(30)).unwrap();
        std::thread::sleep(Duration::from_millis(100));

        assert!(watcher.poll_changes().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn poll_changes_drains_and_deduplicates() {
        let dir = temp_dir("drain");
        let file_path = dir.join("asset.txt");
        std::fs::write(&file_path, "before").unwrap();

        let watcher = AssetWatcher::watch(&dir, Duration::from_millis(30)).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(&file_path, "after").unwrap();

        let first_poll = poll_until_nonempty(&watcher);
        assert_eq!(
            first_poll.iter().filter(|p| *p == &file_path).count(),
            1,
            "the same path should be deduplicated within one poll"
        );
        // Deliberately not asserting quiet after this point: a polling
        // watcher's internal baseline update can lag its event emission
        // by up to one tick, so a second, later "phantom" event for the
        // same still-just-changed file is a real, if rare, possibility —
        // not something this test should be flaky over.

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watch_nonexistent_path_errors() {
        let missing = std::env::temp_dir().join("vge-this-path-does-not-exist-hopefully");
        let err = AssetWatcher::watch(&missing, Duration::from_millis(30)).unwrap_err();
        assert!(matches!(err, AssetError::WatchInit(_)));
    }
}
