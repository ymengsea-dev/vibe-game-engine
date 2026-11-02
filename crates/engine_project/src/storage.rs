//! What a project keeps on disk that Studio can measure and clean.
//!
//! Only rebuildable files are ever deleted. [`scan_project`] lists a
//! project's storage by [`StorageKind`], and [`clean_item`] deletes one item
//! after checking that it is exactly what its kind says:
//!
//! | Kind | What | Deleted by |
//! | --- | --- | --- |
//! | [`StorageKind::BuildCache`] | `<root>/target`, only with Cargo's `CACHEDIR.TAG` | a click, or the automatic limits |
//! | [`StorageKind::StudioTemp`] | `.studio/play-output`, a `.studio/crash.log` older than 30 days | a click, or the automatic limits |
//! | [`StorageKind::OldEngineCopy`] | `sdk/<version>` that `Cargo.toml` no longer uses | a click only |
//! | [`StorageKind::Backup`] | `*.bak` files Studio made before changing a user file | never (shown only) |
//!
//! Scenes, assets, scripts, asset IDs (`.studio/assets`), sessions,
//! autosaves and game saves are never listed at all. Deleting moves the item
//! into `.studio/trash-<id>` first, so a crash halfway never leaves a
//! half-deleted build folder in the project.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Cargo's build folder inside a project.
pub const BUILD_CACHE_DIR: &str = "target";

/// The marker Cargo writes into every build folder it owns.
pub const CARGO_CACHE_TAG: &str = "CACHEDIR.TAG";

/// A crash log older than this is temporary clutter.
pub const CRASH_LOG_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Prefix of the folder an item is moved into just before it is deleted.
const TRASH_PREFIX: &str = "trash-";

/// The kinds of storage Studio reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StorageKind {
    /// Compiled game code (Rust projects). Rebuilt on the next Play or Run.
    BuildCache,
    /// Studio's leftovers: the Play staging folder and old crash logs.
    StudioTemp,
    /// Engine SDK copies left behind by an SDK upgrade.
    OldEngineCopy,
    /// Copies of user files Studio made before changing them.
    Backup,
}

impl StorageKind {
    /// All kinds, in display order.
    pub const ALL: [Self; 4] = [
        Self::BuildCache,
        Self::StudioTemp,
        Self::OldEngineCopy,
        Self::Backup,
    ];

    /// Name for the Storage window.
    pub fn label(self) -> &'static str {
        match self {
            Self::BuildCache => "Build cache",
            Self::StudioTemp => "Temp files",
            Self::OldEngineCopy => "Old engine copies",
            Self::Backup => "Backups",
        }
    }

    /// What deleting it means, for the Storage window.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::BuildCache => "Rebuilt on your next Play or Run",
            Self::StudioTemp => "Leftovers from Play and old crash reports",
            Self::OldEngineCopy => "Left behind by Upgrade Rust SDK",
            Self::Backup => "Copies of files Studio changed",
        }
    }

    /// Whether Studio may delete items of this kind (backups: never).
    pub fn cleanable(self) -> bool {
        !matches!(self, Self::Backup)
    }
}

/// One measured file or folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageItem {
    /// What it is.
    pub kind: StorageKind,
    /// Where it is (absolute).
    pub path: PathBuf,
    /// Its size on disk, in bytes.
    pub bytes: u64,
}

/// Everything [`scan_project`] found in one project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectStorage {
    /// The project root.
    pub root: PathBuf,
    /// The items found, in [`StorageKind::ALL`] order.
    pub items: Vec<StorageItem>,
}

impl ProjectStorage {
    /// Total bytes of `kind`.
    pub fn bytes(&self, kind: StorageKind) -> u64 {
        self.items
            .iter()
            .filter(|item| item.kind == kind)
            .map(|item| item.bytes)
            .sum()
    }

    /// The items of `kind`.
    pub fn items_of(&self, kind: StorageKind) -> impl Iterator<Item = &StorageItem> {
        self.items.iter().filter(move |item| item.kind == kind)
    }
}

/// Why an item was not deleted.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Backups are never deleted by Studio.
    #[error("{0} is a backup; Studio never deletes backups")]
    NotCleanable(PathBuf),
    /// The path is not where an item of its kind may be.
    #[error("{0} is not a cleanable location in this project")]
    NotAllowed(PathBuf),
    /// A build folder without Cargo's marker might not be Cargo's.
    #[error("{0} has no CACHEDIR.TAG, so it may not be a Cargo build folder")]
    NotABuildCache(PathBuf),
    /// The engine copy is still referenced by `Cargo.toml`.
    #[error("{0} is still used by Cargo.toml")]
    InUse(PathBuf),
    /// Moving or deleting failed.
    #[error("could not delete {path}: {source}")]
    Io {
        /// What was being deleted.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

/// Size on disk of `path`: a file's length, or a folder's files summed.
/// Symbolic links are counted as themselves, never followed, so a link
/// out of the project is not measured. Unreadable entries count as zero.
pub fn disk_size(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    let mut total = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    total
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn is_real_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// Whether `Cargo.toml` at `root` refers to `sdk/<version>`.
fn sdk_in_use(root: &Path, version: &str) -> bool {
    std::fs::read_to_string(root.join("Cargo.toml"))
        .is_ok_and(|manifest| manifest.contains(&format!("sdk/{version}/")))
}

fn file_age(path: &Path, now: SystemTime) -> Option<Duration> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    now.duration_since(modified).ok()
}

/// Lists and measures everything cleanable or worth showing in the project
/// at `root`. Never fails: what cannot be read is left out.
pub fn scan_project(root: &Path, now: SystemTime) -> ProjectStorage {
    let mut items = Vec::new();
    let mut add = |kind, path: PathBuf| {
        let bytes = disk_size(&path);
        items.push(StorageItem { kind, path, bytes });
    };

    let target = root.join(BUILD_CACHE_DIR);
    if is_real_dir(&target) && target.join(CARGO_CACHE_TAG).is_file() {
        add(StorageKind::BuildCache, target);
    }

    let studio = root.join(crate::STUDIO_DIR);
    let play_output = studio.join("play-output");
    if is_real_dir(&play_output) {
        add(StorageKind::StudioTemp, play_output);
    }
    let crash_log = studio.join("crash.log");
    if is_real_file(&crash_log)
        && file_age(&crash_log, now).is_some_and(|age| age > CRASH_LOG_MAX_AGE)
    {
        add(StorageKind::StudioTemp, crash_log);
    }

    if let Ok(versions) = std::fs::read_dir(root.join("sdk")) {
        let mut unused: Vec<PathBuf> = versions
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_real_dir(path))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|version| !sdk_in_use(root, version))
            })
            .collect();
        unused.sort();
        for path in unused {
            add(StorageKind::OldEngineCopy, path);
        }
    }

    let mut backups: Vec<PathBuf> = [root.to_path_buf(), root.join("src")]
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten().map(|entry| entry.path()))
        .filter(|path| is_real_file(path) && path.extension().is_some_and(|ext| ext == "bak"))
        .collect();
    backups.sort();
    for path in backups {
        add(StorageKind::Backup, path);
    }

    ProjectStorage {
        root: root.to_path_buf(),
        items,
    }
}

/// Checks that `item` is exactly a cleanable location of its kind inside
/// the project at `root`, and returns its path. Every rule is checked on
/// the real filesystem, without following symbolic links.
fn check(root: &Path, item: &StorageItem) -> Result<PathBuf, StorageError> {
    let path = &item.path;
    let refuse = || StorageError::NotAllowed(path.clone());
    if !item.kind.cleanable() {
        return Err(StorageError::NotCleanable(path.clone()));
    }
    let root = std::fs::canonicalize(root).map_err(|_| refuse())?;
    // The item itself must not be a link; its parent is resolved, so a
    // link higher up cannot point the deletion outside the project.
    let metadata = std::fs::symlink_metadata(path).map_err(|_| refuse())?;
    if metadata.file_type().is_symlink() {
        return Err(refuse());
    }
    let name = path.file_name().ok_or_else(refuse)?;
    let parent = std::fs::canonicalize(path.parent().ok_or_else(refuse)?).map_err(|_| refuse())?;
    let studio = root.join(crate::STUDIO_DIR);
    match item.kind {
        StorageKind::BuildCache => {
            if parent != root || name != BUILD_CACHE_DIR || !metadata.is_dir() {
                return Err(refuse());
            }
            if !path.join(CARGO_CACHE_TAG).is_file() {
                return Err(StorageError::NotABuildCache(path.clone()));
            }
        }
        StorageKind::StudioTemp => {
            let allowed = parent == studio
                && ((name == "play-output" && metadata.is_dir())
                    || (name == "crash.log" && metadata.is_file()));
            if !allowed {
                return Err(refuse());
            }
        }
        StorageKind::OldEngineCopy => {
            if parent != root.join("sdk") || !metadata.is_dir() {
                return Err(refuse());
            }
            let version = name.to_str().ok_or_else(refuse)?;
            if sdk_in_use(&root, version) {
                return Err(StorageError::InUse(path.clone()));
            }
        }
        StorageKind::Backup => return Err(StorageError::NotCleanable(path.clone())),
    }
    Ok(parent.join(name))
}

/// Deletes `item` from the project at `root` after [`check`]ing it, and
/// returns the bytes freed. The item is first moved into
/// `.studio/trash-<id>`, then removed.
///
/// # Errors
///
/// [`StorageError`] if the item is not cleanable where it is, or the
/// filesystem refuses.
pub fn clean_item(root: &Path, item: &StorageItem) -> Result<u64, StorageError> {
    let path = check(root, item)?;
    let bytes = disk_size(&path);
    let io = |source| StorageError::Io {
        path: path.clone(),
        source,
    };
    let studio = path
        .ancestors()
        .find(|ancestor| {
            ancestor
                .file_name()
                .is_some_and(|name| name == crate::STUDIO_DIR)
        })
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.join(crate::STUDIO_DIR));
    std::fs::create_dir_all(&studio).map_err(io)?;
    let trash = studio.join(format!("{TRASH_PREFIX}{}", uuid::Uuid::new_v4().simple()));
    std::fs::rename(&path, &trash).map_err(io)?;
    let removed = if trash.is_dir() {
        std::fs::remove_dir_all(&trash)
    } else {
        std::fs::remove_file(&trash)
    };
    removed.map_err(io)?;
    Ok(bytes)
}

/// Finishes deletions a crash interrupted: removes leftover
/// `.studio/trash-*` entries. Returns the bytes freed.
pub fn empty_trash(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root.join(crate::STUDIO_DIR)) else {
        return 0;
    };
    let mut freed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_trash = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(TRASH_PREFIX));
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !is_trash || metadata.file_type().is_symlink() {
            continue;
        }
        let bytes = disk_size(&path);
        let removed = if metadata.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if removed.is_ok() {
            freed += bytes;
        }
    }
    freed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "vge-storage-{tag}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(root.join(".studio/assets")).expect("project");
        std::fs::write(
            root.join("Cargo.toml"),
            "[dependencies]\nengine = { path = \"sdk/0.2.0/crates/engine\" }\n",
        )
        .expect("manifest");
        root
    }

    fn write(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("dir");
        }
        std::fs::write(path, vec![7u8; bytes]).expect("file");
    }

    fn old(path: &Path) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open");
        file.set_modified(SystemTime::now() - CRASH_LOG_MAX_AGE - Duration::from_secs(60))
            .expect("age");
    }

    #[test]
    fn a_scan_finds_only_rebuildable_items_and_backups() {
        let root = project("scan");
        write(&root.join("target/CACHEDIR.TAG"), 10);
        write(&root.join("target/debug/game"), 1000);
        write(&root.join(".studio/play-output/x"), 5);
        write(&root.join(".studio/crash.log"), 3);
        old(&root.join(".studio/crash.log"));
        write(&root.join("sdk/0.1.0/crates/engine/lib.rs"), 20);
        write(&root.join("sdk/0.2.0/crates/engine/lib.rs"), 20);
        write(&root.join("project.ron.v1.bak"), 4);
        write(&root.join("src/studio_api.lua.legacy.bak"), 6);
        // Never listed:
        write(&root.join(".studio/assets/a.png.meta"), 9);
        write(&root.join(".studio/autosave.ron"), 9);
        write(&root.join(".studio/play-save.ron"), 9);
        write(&root.join("scenes/main.ron"), 9);

        let storage = scan_project(&root, SystemTime::now());
        assert_eq!(storage.bytes(StorageKind::BuildCache), 1010);
        assert_eq!(storage.bytes(StorageKind::StudioTemp), 8);
        let engines: Vec<_> = storage.items_of(StorageKind::OldEngineCopy).collect();
        assert_eq!(engines.len(), 1, "0.2.0 is still in Cargo.toml");
        assert!(engines[0].path.ends_with("sdk/0.1.0"));
        assert_eq!(storage.bytes(StorageKind::Backup), 10);
        for item in &storage.items {
            let text = item.path.to_string_lossy();
            assert!(!text.contains("assets") && !text.contains("save") && !text.contains("scenes"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_recent_crash_log_and_an_untagged_target_are_left_alone() {
        let root = project("young");
        write(&root.join(".studio/crash.log"), 3);
        write(&root.join("target/notes.txt"), 3);
        let storage = scan_project(&root, SystemTime::now());
        assert!(storage.items.is_empty(), "{:?}", storage.items);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cleaning_removes_exactly_the_item_and_reports_its_size() {
        let root = project("clean");
        write(&root.join("target/CACHEDIR.TAG"), 10);
        write(&root.join("target/debug/game"), 90);
        write(&root.join("scenes/main.ron"), 9);
        let storage = scan_project(&root, SystemTime::now());
        let cache = storage
            .items_of(StorageKind::BuildCache)
            .next()
            .expect("cache")
            .clone();
        assert_eq!(clean_item(&root, &cache).expect("clean"), 100);
        assert!(!root.join("target").exists());
        assert!(
            root.join("scenes/main.ron").is_file(),
            "nothing else is touched"
        );
        assert!(
            std::fs::read_dir(root.join(".studio"))
                .expect("studio")
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().starts_with("trash-")),
            "the trash is emptied"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cleaning_refuses_anything_that_is_not_what_it_claims() {
        let root = project("refuse");
        write(&root.join("scenes/main.ron"), 9);
        write(&root.join("project.ron.v1.bak"), 4);
        write(&root.join("target/notes.txt"), 3);
        write(&root.join("sdk/0.2.0/crates/engine/lib.rs"), 20);
        let item = |kind, path: PathBuf| StorageItem {
            kind,
            path,
            bytes: 0,
        };
        let refused = [
            item(StorageKind::BuildCache, root.join("scenes")),
            item(StorageKind::BuildCache, root.join("target")),
            item(StorageKind::StudioTemp, root.join("scenes/main.ron")),
            item(StorageKind::Backup, root.join("project.ron.v1.bak")),
            item(StorageKind::OldEngineCopy, root.join("sdk/0.2.0")),
            item(StorageKind::BuildCache, std::env::temp_dir()),
        ];
        for item in &refused {
            assert!(clean_item(&root, item).is_err(), "{item:?} must be refused");
        }
        assert!(root.join("scenes/main.ron").is_file());
        assert!(root.join("project.ron.v1.bak").is_file());
        assert!(
            root.join("target/notes.txt").is_file(),
            "no CACHEDIR.TAG, not Cargo's"
        );
        assert!(root.join("sdk/0.2.0").is_dir(), "still in Cargo.toml");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_cannot_redirect_a_deletion_outside_the_project() {
        let root = project("link");
        let outside = project("outside");
        write(&outside.join("target/CACHEDIR.TAG"), 10);
        std::os::unix::fs::symlink(outside.join("target"), root.join("target")).expect("link");
        let storage = scan_project(&root, SystemTime::now());
        assert!(storage.items_of(StorageKind::BuildCache).next().is_none());
        let item = StorageItem {
            kind: StorageKind::BuildCache,
            path: root.join("target"),
            bytes: 0,
        };
        assert!(clean_item(&root, &item).is_err());
        assert!(outside.join("target/CACHEDIR.TAG").is_file());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
    }

    #[test]
    fn interrupted_deletions_are_finished_later() {
        let root = project("trash");
        write(&root.join(".studio/trash-abc/big"), 50);
        write(&root.join(".studio/session.ron"), 5);
        assert_eq!(empty_trash(&root), 50);
        assert!(!root.join(".studio/trash-abc").exists());
        assert!(root.join(".studio/session.ron").is_file());
        let _ = std::fs::remove_dir_all(root);
    }
}
