//! Asset metadata sidecars and an import cache.
//!
//! Every source asset file (`foo.png`, `bar.gltf`, ...) gets a `.meta`
//! sidecar (`foo.png.meta`) holding a stable [`AssetId`] plus a hash of
//! the source content at the time it was last imported. [`ImportCache`]
//! reads those sidecars and answers "has this file changed since I last
//! saw it?" — so a re-import (decode, mip generation, ...) is skipped for
//! an unchanged file.
//!
//! The hash is a plain non-cryptographic [`std::hash`] digest of the
//! file bytes: enough to detect edits, not a security boundary.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AssetError;

/// A non-cryptographic 64-bit digest of `bytes`, stable across runs
/// (`DefaultHasher` is seeded with fixed keys).
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, AssetError> {
    std::fs::read(path).map_err(|err| AssetError::MetaIo {
        path: path.display().to_string(),
        message: err.to_string(),
    })
}

/// The `.meta` sidecar for one source asset: a stable identity and the
/// content hash recorded at the last import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetMeta {
    /// Stable id for this asset — survives edits and renames (a rename
    /// carries the `.meta` alongside).
    pub id: Uuid,
    /// Hash of the source file's bytes as of the last [`AssetMeta::refresh`].
    pub source_hash: u64,
}

impl AssetMeta {
    /// Sidecar file extension, appended to the full asset filename:
    /// `sprite.png` → `sprite.png.meta`.
    pub const EXTENSION: &'static str = "meta";

    /// The sidecar path for `asset_path` (its filename plus
    /// `.`[`AssetMeta::EXTENSION`]).
    pub fn sidecar_path(asset_path: &Path) -> PathBuf {
        let mut name = asset_path.as_os_str().to_os_string();
        name.push(".");
        name.push(Self::EXTENSION);
        PathBuf::from(name)
    }

    /// Loads `asset_path`'s sidecar, or — if none exists — creates one
    /// with a fresh id and the file's current hash and writes it to disk.
    ///
    /// # Errors
    ///
    /// [`AssetError::MetaIo`] if the source or sidecar can't be read/
    /// written; [`AssetError::MetaParse`] if an existing sidecar is
    /// malformed.
    pub fn load_or_create(asset_path: &Path) -> Result<Self, AssetError> {
        let sidecar = Self::sidecar_path(asset_path);
        if sidecar.exists() {
            let text = std::fs::read_to_string(&sidecar).map_err(|err| AssetError::MetaIo {
                path: sidecar.display().to_string(),
                message: err.to_string(),
            })?;
            return ron::from_str(&text).map_err(|err| AssetError::MetaParse {
                path: sidecar.display().to_string(),
                reason: err.to_string(),
            });
        }

        let meta = Self {
            id: Uuid::new_v4(),
            source_hash: hash_bytes(&read_bytes(asset_path)?),
        };
        meta.write(asset_path)?;
        Ok(meta)
    }

    /// Whether `asset_path`'s current content hash differs from this
    /// meta's `source_hash` (i.e. the source changed since the last
    /// import).
    ///
    /// # Errors
    ///
    /// [`AssetError::MetaIo`] if the source file can't be read.
    pub fn is_stale(&self, asset_path: &Path) -> Result<bool, AssetError> {
        Ok(hash_bytes(&read_bytes(asset_path)?) != self.source_hash)
    }

    /// Recomputes `source_hash` from `asset_path` and rewrites the
    /// sidecar. The `id` is preserved.
    ///
    /// # Errors
    ///
    /// [`AssetError::MetaIo`] on a read/write failure.
    pub fn refresh(&mut self, asset_path: &Path) -> Result<(), AssetError> {
        self.source_hash = hash_bytes(&read_bytes(asset_path)?);
        self.write(asset_path)
    }

    fn write(&self, asset_path: &Path) -> Result<(), AssetError> {
        let sidecar = Self::sidecar_path(asset_path);
        let text =
            ron::ser::to_string_pretty(self, ron::ser::PrettyConfig::default()).map_err(|err| {
                AssetError::MetaParse {
                    path: sidecar.display().to_string(),
                    reason: err.to_string(),
                }
            })?;
        std::fs::write(&sidecar, text).map_err(|err| AssetError::MetaIo {
            path: sidecar.display().to_string(),
            message: err.to_string(),
        })
    }
}

/// An in-memory index of [`AssetMeta`] sidecars, keyed by source path —
/// the import step consults it to skip work for unchanged files.
#[derive(Debug, Default)]
pub struct ImportCache {
    entries: HashMap<PathBuf, AssetMeta>,
}

impl ImportCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensures `asset_path` has an up-to-date `.meta` sidecar, caching it,
    /// and reports whether the source needs (re)importing.
    ///
    /// Returns `(id, needs_import)`:
    /// - `needs_import` is `true` if the sidecar was just created, or the
    ///   source's hash no longer matches it (in which case the sidecar is
    ///   refreshed);
    /// - `false` if a matching sidecar was already on disk / in the cache.
    ///
    /// # Errors
    ///
    /// Propagates [`AssetError::MetaIo`]/[`AssetError::MetaParse`].
    pub fn ensure(&mut self, asset_path: &Path) -> Result<(Uuid, bool), AssetError> {
        if let Some(meta) = self.entries.get_mut(asset_path) {
            if meta.is_stale(asset_path)? {
                meta.refresh(asset_path)?;
                return Ok((meta.id, true));
            }
            return Ok((meta.id, false));
        }

        let sidecar_existed = AssetMeta::sidecar_path(asset_path).exists();
        let mut meta = AssetMeta::load_or_create(asset_path)?;
        let mut needs_import = !sidecar_existed;
        if sidecar_existed && meta.is_stale(asset_path)? {
            meta.refresh(asset_path)?;
            needs_import = true;
        }
        let id = meta.id;
        self.entries.insert(asset_path.to_path_buf(), meta);
        Ok((id, needs_import))
    }

    /// The cached meta for `asset_path`, if [`ImportCache::ensure`] has
    /// been called for it.
    pub fn get(&self, asset_path: &Path) -> Option<&AssetMeta> {
        self.entries.get(asset_path)
    }

    /// Number of assets currently tracked.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is tracked yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory under the OS temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("vge_meta_test_{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hash_bytes_is_deterministic_and_content_sensitive() {
        assert_eq!(hash_bytes(b"hello"), hash_bytes(b"hello"));
        assert_ne!(hash_bytes(b"hello"), hash_bytes(b"world"));
    }

    #[test]
    fn sidecar_path_appends_the_extension_to_the_full_name() {
        assert_eq!(
            AssetMeta::sidecar_path(Path::new("art/sprite.png")),
            PathBuf::from("art/sprite.png.meta")
        );
    }

    #[test]
    fn asset_meta_round_trips_through_ron() {
        let meta = AssetMeta {
            id: Uuid::new_v4(),
            source_hash: 0x1234_5678_9abc_def0,
        };
        let text = ron::ser::to_string(&meta).unwrap();
        let parsed: AssetMeta = ron::from_str(&text).unwrap();
        assert_eq!(parsed, meta);
    }

    #[test]
    fn load_or_create_writes_a_sidecar_the_first_time() {
        let dir = TempDir::new();
        let asset = dir.file("tex.bin", b"pixels");

        let meta = AssetMeta::load_or_create(&asset).unwrap();
        assert!(AssetMeta::sidecar_path(&asset).exists());
        assert_eq!(meta.source_hash, hash_bytes(b"pixels"));

        // Second call reads the same one back — id is stable.
        let again = AssetMeta::load_or_create(&asset).unwrap();
        assert_eq!(again.id, meta.id);
    }

    #[test]
    fn is_stale_tracks_source_edits() {
        let dir = TempDir::new();
        let asset = dir.file("mesh.bin", b"v1");
        let meta = AssetMeta::load_or_create(&asset).unwrap();
        assert!(!meta.is_stale(&asset).unwrap());

        std::fs::write(&asset, b"v2 changed").unwrap();
        assert!(meta.is_stale(&asset).unwrap());
    }

    #[test]
    fn import_cache_reports_new_then_unchanged_then_reimport() {
        let dir = TempDir::new();
        let asset = dir.file("clip.bin", b"take one");
        let mut cache = ImportCache::new();

        let (id1, needs1) = cache.ensure(&asset).unwrap();
        assert!(needs1, "first sight of a file needs an import");

        let (id2, needs2) = cache.ensure(&asset).unwrap();
        assert_eq!(id2, id1, "id is stable");
        assert!(!needs2, "unchanged file skips re-import");

        std::fs::write(&asset, b"take two, longer").unwrap();
        let (id3, needs3) = cache.ensure(&asset).unwrap();
        assert_eq!(id3, id1);
        assert!(needs3, "an edited file needs re-import");

        // ...and is then considered current again.
        let (_, needs4) = cache.ensure(&asset).unwrap();
        assert!(!needs4);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn import_cache_picks_up_a_pre_existing_sidecar_without_reimport() {
        let dir = TempDir::new();
        let asset = dir.file("preexisting.bin", b"already imported");
        // Sidecar written by an earlier run.
        AssetMeta::load_or_create(&asset).unwrap();

        let mut cache = ImportCache::new();
        let (_, needs) = cache.ensure(&asset).unwrap();
        assert!(!needs, "a matching sidecar means no re-import");
    }

    #[test]
    fn ensure_errors_on_a_missing_source_file() {
        let mut cache = ImportCache::new();
        let missing = std::env::temp_dir().join("vge_meta_test_does_not_exist.bin");
        assert!(matches!(
            cache.ensure(&missing),
            Err(AssetError::MetaIo { .. })
        ));
    }
}
