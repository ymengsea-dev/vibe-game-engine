//! Storage management: what Studio's projects keep on disk, cleaning it,
//! and the optional automatic limits.
//!
//! - [`StudioSettings`] (per user) holds the limits: off by default, 10 GB
//!   of build caches, 30 days unused.
//! - [`StorageLedger`] (per user) remembers every project Studio opened and
//!   when. Cleaning only ever looks at these projects; Studio never searches
//!   the disk.
//! - [`plan_automatic_cleanup`] decides what the limits remove (pure, tested).
//! - [`StorageWorker`] measures and deletes on a background thread, so the
//!   Storage window never freezes Studio.
//!
//! What may be deleted, and the safety checks, live in
//! [`engine_project::storage`].

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use engine_project::storage::{self as disk, ProjectStorage, StorageItem, StorageKind};
use serde::{Deserialize, Serialize};

use crate::error::EditorError;

/// Bytes in one gigabyte, as the Storage window counts them.
pub const GIGABYTE: u64 = 1_000_000_000;

/// Seconds in a day.
const DAY_SECONDS: u64 = 24 * 60 * 60;

/// How often the automatic limits run while Studio stays open.
pub const AUTOMATIC_INTERVAL: Duration = Duration::from_secs(DAY_SECONDS);

/// The automatic limits. Off until the user turns them on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageSettings {
    /// Whether Studio cleans by itself.
    pub automatic: bool,
    /// Keep all build caches together under this many GB.
    pub max_build_cache_gb: u32,
    /// Remove a project's build cache after this many days unopened.
    pub unused_days: u32,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            automatic: false,
            max_build_cache_gb: 10,
            unused_days: 30,
        }
    }
}

/// Per-user Studio settings, `settings.ron` in the user's config folder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StudioSettings {
    /// Storage limits.
    pub storage: StorageSettings,
}

/// One project Studio has opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// The project root.
    pub root: PathBuf,
    /// When Studio last opened it, in seconds since 1970.
    pub last_opened: u64,
}

/// Every project Studio has opened, `storage.ron` in the user's config
/// folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageLedger {
    /// Most recently opened first.
    pub projects: Vec<LedgerEntry>,
}

impl StorageLedger {
    /// Records that `root` was opened at `now`.
    pub fn touch(&mut self, root: &Path, now: SystemTime) {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        self.projects.retain(|entry| entry.root != root);
        self.projects.insert(
            0,
            LedgerEntry {
                root,
                last_opened: seconds(now),
            },
        );
    }

    /// Forgets projects whose folder no longer exists.
    pub fn forget_missing(&mut self) {
        self.projects.retain(|entry| entry.root.is_dir());
    }
}

/// `time` in whole seconds since 1970 (0 before it).
pub fn seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The user's Studio config folder, where per-user files live.
pub fn config_dir() -> Option<PathBuf> {
    crate::RecentProjects::default_path()?
        .parent()
        .map(Path::to_path_buf)
}

fn load_ron<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    let Ok(text) = std::fs::read_to_string(path) else {
        return T::default();
    };
    ron::from_str(&text).unwrap_or_else(|err| {
        tracing::warn!(error = %err, path = %path.display(), "unreadable settings; using defaults");
        T::default()
    })
}

fn save_ron<T: Serialize>(value: &T, path: &Path) -> Result<(), EditorError> {
    let failed = |message: String| EditorError::Session {
        path: path.to_path_buf(),
        message,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| failed(err.to_string()))?;
    }
    let text = ron::ser::to_string_pretty(value, ron::ser::PrettyConfig::default())
        .map_err(|err| failed(err.to_string()))?;
    engine_scene::write_atomic(path, text.as_bytes()).map_err(|err| failed(err.to_string()))
}

impl StudioSettings {
    /// Reads `path`; missing or unreadable means the defaults.
    pub fn load(path: &Path) -> Self {
        load_ron(path)
    }

    /// Writes `path`.
    ///
    /// # Errors
    ///
    /// [`EditorError::Session`] if it cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), EditorError> {
        save_ron(self, path)
    }
}

impl StorageLedger {
    /// Reads `path`; missing or unreadable means empty.
    pub fn load(path: &Path) -> Self {
        load_ron(path)
    }

    /// Writes `path`.
    ///
    /// # Errors
    ///
    /// [`EditorError::Session`] if it cannot be written.
    pub fn save(&self, path: &Path) -> Result<(), EditorError> {
        save_ron(self, path)
    }
}

/// Why the automatic limits remove something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupReason {
    /// The project was not opened for this many days.
    Unused {
        /// Days since it was last opened.
        days: u64,
    },
    /// All build caches together were over the cap.
    OverCap,
    /// Studio leftovers, always safe.
    Temporary,
}

/// One deletion the automatic limits want.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedCleanup {
    /// The project it belongs to.
    pub root: PathBuf,
    /// What to delete.
    pub item: StorageItem,
    /// Why.
    pub reason: CleanupReason,
}

/// What the automatic limits remove, given each known project's storage
/// and when it was last opened. The `current` project is never touched.
/// Build caches go first for projects unused longer than the limit, then,
/// least recently opened first, until all caches fit under the cap.
pub fn plan_automatic_cleanup(
    projects: &[(LedgerEntry, ProjectStorage)],
    current: &Path,
    now: SystemTime,
    settings: StorageSettings,
) -> Vec<PlannedCleanup> {
    if !settings.automatic {
        return Vec::new();
    }
    let now = seconds(now);
    let mut plan = Vec::new();
    let mut total: u64 = projects
        .iter()
        .map(|(_, storage)| storage.bytes(StorageKind::BuildCache))
        .sum();
    let cap = u64::from(settings.max_build_cache_gb) * GIGABYTE;
    let mut candidates: Vec<&(LedgerEntry, ProjectStorage)> = projects
        .iter()
        .filter(|(entry, _)| entry.root != current)
        .collect();
    // Least recently opened first.
    candidates.sort_by_key(|(entry, _)| entry.last_opened);
    for (entry, storage) in &candidates {
        for item in storage.items_of(StorageKind::StudioTemp) {
            plan.push(PlannedCleanup {
                root: entry.root.clone(),
                item: item.clone(),
                reason: CleanupReason::Temporary,
            });
        }
        let days = now.saturating_sub(entry.last_opened) / DAY_SECONDS;
        for item in storage.items_of(StorageKind::BuildCache) {
            let reason = if days >= u64::from(settings.unused_days) {
                CleanupReason::Unused { days }
            } else if total > cap {
                CleanupReason::OverCap
            } else {
                continue;
            };
            total = total.saturating_sub(item.bytes);
            plan.push(PlannedCleanup {
                root: entry.root.clone(),
                item: item.clone(),
                reason,
            });
        }
    }
    plan
}

/// Work for the [`StorageWorker`].
#[derive(Debug, Clone)]
enum Job {
    Measure(Vec<LedgerEntry>),
    Clean {
        root: PathBuf,
        item: StorageItem,
    },
    Automatic {
        projects: Vec<LedgerEntry>,
        current: PathBuf,
        settings: StorageSettings,
    },
}

/// What the [`StorageWorker`] reports back.
#[derive(Debug)]
pub enum StorageEvent {
    /// Every known project measured, most recently opened first.
    Measured(Vec<(LedgerEntry, ProjectStorage)>),
    /// An item was deleted; `bytes` were freed.
    Cleaned {
        /// The project.
        root: PathBuf,
        /// What was deleted.
        item: StorageItem,
        /// Bytes freed.
        bytes: u64,
        /// Why, when the automatic limits did it.
        reason: Option<CleanupReason>,
    },
    /// A deletion was refused or failed.
    Failed(String),
}

/// Measures and cleans on a background thread.
pub struct StorageWorker {
    jobs: Sender<Job>,
    events: Receiver<StorageEvent>,
}

impl StorageWorker {
    /// Starts the worker thread.
    ///
    /// # Errors
    ///
    /// A readable message if the thread could not start.
    pub fn start() -> Result<Self, String> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (outbox, events) = mpsc::channel();
        std::thread::Builder::new()
            .name("studio-storage".to_owned())
            .spawn(move || {
                for job in inbox {
                    if !run(job, &outbox) {
                        break;
                    }
                }
            })
            .map_err(|err| format!("could not start the storage worker: {err}"))?;
        Ok(Self { jobs, events })
    }

    /// Measures every project in `projects`.
    pub fn measure(&self, projects: Vec<LedgerEntry>) {
        let _ = self.jobs.send(Job::Measure(projects));
    }

    /// Deletes one item, after the safety checks.
    pub fn clean(&self, root: PathBuf, item: StorageItem) {
        let _ = self.jobs.send(Job::Clean { root, item });
    }

    /// Applies the automatic limits to `projects` (never to `current`).
    pub fn automatic(
        &self,
        projects: Vec<LedgerEntry>,
        current: PathBuf,
        settings: StorageSettings,
    ) {
        let _ = self.jobs.send(Job::Automatic {
            projects,
            current,
            settings,
        });
    }

    /// Results since the last call.
    pub fn poll(&self) -> Vec<StorageEvent> {
        self.events.try_iter().collect()
    }
}

fn measure(projects: &[LedgerEntry]) -> Vec<(LedgerEntry, ProjectStorage)> {
    let now = SystemTime::now();
    projects
        .iter()
        .filter(|entry| entry.root.is_dir())
        .map(|entry| {
            disk::empty_trash(&entry.root);
            (entry.clone(), disk::scan_project(&entry.root, now))
        })
        .collect()
}

fn clean(root: PathBuf, item: StorageItem, reason: Option<CleanupReason>) -> StorageEvent {
    match disk::clean_item(&root, &item) {
        Ok(bytes) => StorageEvent::Cleaned {
            root,
            item,
            bytes,
            reason,
        },
        Err(err) => StorageEvent::Failed(err.to_string()),
    }
}

/// Runs one job; `false` once Studio stopped listening.
fn run(job: Job, outbox: &Sender<StorageEvent>) -> bool {
    match job {
        Job::Measure(projects) => outbox
            .send(StorageEvent::Measured(measure(&projects)))
            .is_ok(),
        Job::Clean { root, item } => outbox.send(clean(root, item, None)).is_ok(),
        Job::Automatic {
            projects,
            current,
            settings,
        } => {
            let measured = measure(&projects);
            for planned in plan_automatic_cleanup(&measured, &current, SystemTime::now(), settings)
            {
                if outbox
                    .send(clean(planned.root, planned.item, Some(planned.reason)))
                    .is_err()
                {
                    return false;
                }
            }
            outbox
                .send(StorageEvent::Measured(measure(&projects)))
                .is_ok()
        }
    }
}

/// The Storage window's state: the ledger, the latest measurements, and
/// the worker, started the first time it is needed.
#[derive(Default)]
pub struct StorageView {
    /// Every project Studio has opened.
    pub ledger: StorageLedger,
    /// The latest measurement of every known project.
    pub measured: Option<Vec<(LedgerEntry, ProjectStorage)>>,
    /// Whether a measurement or cleanup is under way.
    pub busy: bool,
    /// A file the user asked to see in Finder (a backup).
    pub reveal: Option<PathBuf>,
    /// Old engine copies waiting for the user to confirm their deletion.
    pub confirm: Option<(PathBuf, Vec<StorageItem>)>,
    worker: Option<StorageWorker>,
    /// The ledger or settings changed and should be written.
    save_needed: bool,
    started: Option<std::time::Instant>,
    last_automatic: Option<std::time::Instant>,
}

impl StorageView {
    fn worker(&mut self) -> Option<&StorageWorker> {
        if self.worker.is_none() {
            match StorageWorker::start() {
                Ok(worker) => self.worker = Some(worker),
                Err(message) => tracing::warn!(%message, "storage management unavailable"),
            }
        }
        self.worker.as_ref()
    }

    /// Records that `root` was opened now.
    pub fn opened(&mut self, root: &Path) {
        self.ledger.touch(root, SystemTime::now());
        self.save_needed = true;
    }

    /// Asks for the ledger or settings to be written (after a change).
    pub fn mark_changed(&mut self) {
        self.save_needed = true;
    }

    /// Whether the ledger or settings should be written now; clears it.
    pub fn take_save_needed(&mut self) -> bool {
        std::mem::take(&mut self.save_needed)
    }

    /// Measures every known project again, in the background.
    pub fn refresh(&mut self) {
        self.ledger.forget_missing();
        let projects = self.ledger.projects.clone();
        if let Some(worker) = self.worker() {
            worker.measure(projects);
            self.busy = true;
        }
    }

    /// Deletes one item in the background (after the safety checks).
    pub fn clean(&mut self, root: PathBuf, item: StorageItem) {
        if let Some(worker) = self.worker() {
            worker.clean(root, item);
            self.busy = true;
        }
    }

    /// Runs the automatic limits when they are on and due (a few seconds
    /// after Studio starts, then daily), and applies the worker's results,
    /// writing each deletion to `output`.
    pub fn tick(&mut self, settings: StorageSettings, current: &Path, output: &crate::OutputLog) {
        let now = std::time::Instant::now();
        let started = *self.started.get_or_insert(now);
        let due = match self.last_automatic {
            None => now.duration_since(started) >= Duration::from_secs(5),
            Some(last) => now.duration_since(last) >= AUTOMATIC_INTERVAL,
        };
        if settings.automatic && due {
            self.last_automatic = Some(now);
            self.ledger.forget_missing();
            let projects = self.ledger.projects.clone();
            let current = std::fs::canonicalize(current).unwrap_or_else(|_| current.to_path_buf());
            if let Some(worker) = self.worker() {
                worker.automatic(projects, current, settings);
                self.busy = true;
            }
        }
        let Some(worker) = self.worker.as_ref() else {
            return;
        };
        let mut cleaned = false;
        for event in worker.poll() {
            match event {
                StorageEvent::Measured(measured) => {
                    self.measured = Some(measured);
                    self.busy = false;
                }
                StorageEvent::Cleaned {
                    root,
                    item,
                    bytes,
                    reason,
                } => {
                    let project = root.file_name().map_or_else(
                        || root.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    let why = match reason {
                        Some(CleanupReason::Unused { days }) => {
                            format!(", not opened for {days} days")
                        }
                        Some(CleanupReason::OverCap) => {
                            ", to stay under the storage limit".to_owned()
                        }
                        Some(CleanupReason::Temporary) | None => String::new(),
                    };
                    output.write_line(format!(
                        "[storage] freed {}: {} of '{project}'{why}",
                        format_bytes(bytes),
                        item.kind.label().to_lowercase()
                    ));
                    cleaned = true;
                }
                StorageEvent::Failed(message) => {
                    output.write_line(format!("[storage] not cleaned: {message}"));
                    self.busy = false;
                }
            }
        }
        if cleaned {
            self.refresh();
        }
    }
}

/// A byte count for people: "4.1 GB", "18 KB".
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("GB", GIGABYTE), ("MB", 1_000_000), ("KB", 1_000)];
    for (unit, size) in UNITS {
        if bytes >= size {
            return format!("{:.1} {unit}", bytes as f64 / size as f64);
        }
    }
    format!("{bytes} B")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(root: &str, bytes: u64) -> ProjectStorage {
        ProjectStorage {
            root: PathBuf::from(root),
            items: vec![StorageItem {
                kind: StorageKind::BuildCache,
                path: PathBuf::from(root).join("target"),
                bytes,
            }],
        }
    }

    fn opened(root: &str, days_ago: u64, now: SystemTime) -> LedgerEntry {
        LedgerEntry {
            root: PathBuf::from(root),
            last_opened: seconds(now) - days_ago * DAY_SECONDS,
        }
    }

    fn on() -> StorageSettings {
        StorageSettings {
            automatic: true,
            ..StorageSettings::default()
        }
    }

    #[test]
    fn the_defaults_are_off_with_ten_gigabytes_and_thirty_days() {
        let settings = StorageSettings::default();
        assert!(!settings.automatic);
        assert_eq!(settings.max_build_cache_gb, 10);
        assert_eq!(settings.unused_days, 30);
        let now = SystemTime::now();
        let old = [(opened("/old", 400, now), cache("/old", 50 * GIGABYTE))];
        assert!(
            plan_automatic_cleanup(&old, Path::new("/current"), now, settings).is_empty(),
            "nothing is removed while the limits are off"
        );
    }

    #[test]
    fn unused_projects_lose_their_build_cache_but_the_current_one_never_does() {
        let now = SystemTime::now();
        let projects = [
            (opened("/current", 90, now), cache("/current", GIGABYTE)),
            (opened("/old", 31, now), cache("/old", GIGABYTE)),
            (opened("/recent", 2, now), cache("/recent", GIGABYTE)),
        ];
        let plan = plan_automatic_cleanup(&projects, Path::new("/current"), now, on());
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].root, PathBuf::from("/old"));
        assert_eq!(plan[0].reason, CleanupReason::Unused { days: 31 });
    }

    #[test]
    fn over_the_cap_the_least_recently_opened_caches_go_first() {
        let now = SystemTime::now();
        let projects = [
            (opened("/a", 1, now), cache("/a", 4 * GIGABYTE)),
            (opened("/b", 5, now), cache("/b", 4 * GIGABYTE)),
            (opened("/c", 9, now), cache("/c", 4 * GIGABYTE)),
            (opened("/current", 0, now), cache("/current", 4 * GIGABYTE)),
        ];
        // 16 GB in all, cap 10: removing /c (oldest) and /b gets to 8.
        let plan = plan_automatic_cleanup(&projects, Path::new("/current"), now, on());
        let removed: Vec<_> = plan.iter().map(|planned| planned.root.clone()).collect();
        assert_eq!(removed, [PathBuf::from("/c"), PathBuf::from("/b")]);
        assert!(
            plan.iter()
                .all(|planned| planned.reason == CleanupReason::OverCap)
        );
    }

    #[test]
    fn opening_a_project_moves_it_to_the_front_once() {
        let mut ledger = StorageLedger::default();
        let now = SystemTime::now();
        ledger.touch(Path::new("/nowhere/a"), now);
        ledger.touch(Path::new("/nowhere/b"), now);
        ledger.touch(Path::new("/nowhere/a"), now);
        let roots: Vec<_> = ledger
            .projects
            .iter()
            .map(|entry| entry.root.clone())
            .collect();
        assert_eq!(
            roots,
            [PathBuf::from("/nowhere/a"), PathBuf::from("/nowhere/b")]
        );
        ledger.forget_missing();
        assert!(ledger.projects.is_empty());
    }

    #[test]
    fn settings_and_ledger_round_trip_and_tolerate_garbage() {
        let dir = std::env::temp_dir().join(format!("vge-settings-{}", std::process::id()));
        let settings = StudioSettings {
            storage: StorageSettings {
                automatic: true,
                max_build_cache_gb: 25,
                unused_days: 7,
            },
        };
        settings.save(&dir.join("settings.ron")).expect("save");
        assert_eq!(StudioSettings::load(&dir.join("settings.ron")), settings);
        std::fs::write(dir.join("broken.ron"), "not ron").expect("write");
        assert_eq!(
            StudioSettings::load(&dir.join("broken.ron")),
            StudioSettings::default()
        );
        assert_eq!(
            StorageLedger::load(&dir.join("missing.ron")),
            StorageLedger::default()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(18_000), "18.0 KB");
        assert_eq!(format_bytes(4_100_000_000), "4.1 GB");
    }

    #[test]
    fn the_worker_measures_and_cleans_in_the_background() {
        let root = std::env::temp_dir().join(format!("vge-worker-{}", std::process::id()));
        std::fs::create_dir_all(root.join("target")).expect("dir");
        std::fs::write(root.join("target/CACHEDIR.TAG"), "tag").expect("tag");
        std::fs::write(root.join("target/big"), vec![0u8; 1000]).expect("file");
        let worker = StorageWorker::start().expect("worker");
        let entry = LedgerEntry {
            root: root.clone(),
            last_opened: 0,
        };
        let wait = |worker: &StorageWorker| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let events = worker.poll();
                if !events.is_empty() || std::time::Instant::now() > deadline {
                    return events;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        worker.measure(vec![entry]);
        let events = wait(&worker);
        let Some(StorageEvent::Measured(measured)) = events.into_iter().next() else {
            panic!("no measurement");
        };
        let item = measured[0].1.items[0].clone();
        assert_eq!(item.bytes, 1003);
        worker.clean(root.clone(), item);
        assert!(matches!(
            wait(&worker).first(),
            Some(StorageEvent::Cleaned { bytes: 1003, .. })
        ));
        assert!(!root.join("target").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
