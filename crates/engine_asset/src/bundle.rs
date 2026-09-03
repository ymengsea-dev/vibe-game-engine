//! Runtime asset packaging: fold a project's loose asset files into one
//! `.pak` archive a shipped game reads instead of a directory tree.
//!
//! Format (all integers little-endian):
//!
//! ```text
//! magic         : 8 bytes  = b"VGEPAK01"
//! entry_count   : u32
//! entries[]     : repeated entry_count times
//!   path_len    : u32
//!   path        : path_len bytes, UTF-8, '/'-separated, project-relative
//!   offset      : u64        byte offset into the data section
//!   length      : u64        byte length
//! data section  : the entry bytes, concatenated in `entries` order
//! ```
//!
//! [`pack_dir`] writes one; [`Bundle::open`] reads the whole file into
//! memory and [`Bundle::get`] hands back a slice for a relative path.
//! Bundle files are untrusted input — every parse step is bounds-checked
//! and returns [`AssetError::Bundle`] rather than panicking.

use std::path::{Path, PathBuf};

use crate::error::AssetError;

/// File magic every `.pak` starts with.
pub const BUNDLE_MAGIC: &[u8; 8] = b"VGEPAK01";

fn io_err(context: &str, err: &std::io::Error) -> AssetError {
    AssetError::Bundle(format!("{context}: {err}"))
}

/// One packed file: its archive-relative path and where its bytes sit in
/// the data section.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    path: String,
    offset: u64,
    length: u64,
}

/// Summary of a [`pack_dir`] run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BundleStats {
    /// Number of files packed.
    pub files: usize,
    /// Total byte length of the data section.
    pub bytes: u64,
}

/// Recursively lists every regular file under `dir`, as `/`-separated
/// paths relative to `root`, sorted for a deterministic archive.
fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
) -> Result<(), AssetError> {
    let read_dir =
        std::fs::read_dir(dir).map_err(|err| io_err(&dir.display().to_string(), &err))?;
    let mut children: Vec<PathBuf> = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|err| io_err(&dir.display().to_string(), &err))?;
        children.push(entry.path());
    }
    children.sort();
    for path in children {
        let file_type = std::fs::symlink_metadata(&path)
            .map_err(|err| io_err(&path.display().to_string(), &err))?;
        if file_type.is_dir() {
            collect_files(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path.strip_prefix(root).map_err(|_| {
                AssetError::Bundle(format!("{} escaped the pack root", path.display()))
            })?;
            let mut key = String::new();
            for (i, component) in relative.components().enumerate() {
                if i > 0 {
                    key.push('/');
                }
                key.push_str(&component.as_os_str().to_string_lossy());
            }
            out.push((key, path));
        }
        // Symlinks and specials are skipped (not followed), same stance
        // as the asset browser's directory scan.
    }
    Ok(())
}

/// Packs every file under `src_dir` into a `.pak` archive at `out_path`,
/// overwriting any existing file. Directory structure is preserved as
/// `/`-separated relative paths.
///
/// # Errors
///
/// [`AssetError::Bundle`] if `src_dir` can't be walked, a file can't be
/// read, or the archive can't be written.
pub fn pack_dir(src_dir: &Path, out_path: &Path) -> Result<BundleStats, AssetError> {
    let mut files = Vec::new();
    collect_files(src_dir, src_dir, &mut files)?;

    // First pass: read every file, assign offsets.
    let mut entries: Vec<Entry> = Vec::with_capacity(files.len());
    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(files.len());
    let mut offset: u64 = 0;
    for (key, path) in &files {
        let bytes = std::fs::read(path).map_err(|err| io_err(&path.display().to_string(), &err))?;
        let length = bytes.len() as u64;
        entries.push(Entry {
            path: key.clone(),
            offset,
            length,
        });
        offset += length;
        blobs.push(bytes);
    }

    // Serialize: header + index + data.
    let mut archive = Vec::new();
    archive.extend_from_slice(BUNDLE_MAGIC);
    archive.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in &entries {
        let path_bytes = entry.path.as_bytes();
        archive.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
        archive.extend_from_slice(path_bytes);
        archive.extend_from_slice(&entry.offset.to_le_bytes());
        archive.extend_from_slice(&entry.length.to_le_bytes());
    }
    for blob in &blobs {
        archive.extend_from_slice(blob);
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| io_err(&parent.display().to_string(), &err))?;
    }
    std::fs::write(out_path, &archive)
        .map_err(|err| io_err(&out_path.display().to_string(), &err))?;

    Ok(BundleStats {
        files: entries.len(),
        bytes: offset,
    })
}

/// A read-only, in-memory view of a `.pak` archive.
#[derive(Debug)]
pub struct Bundle {
    /// The whole file. `data`-section slices borrow from here.
    raw: Vec<u8>,
    /// Absolute byte offset of the data section within `raw`.
    data_start: usize,
    entries: Vec<Entry>,
}

/// Reads a little-endian `u32` at `cursor`, advancing it.
fn take_u32(raw: &[u8], cursor: &mut usize) -> Result<u32, AssetError> {
    let end = cursor
        .checked_add(4)
        .filter(|end| *end <= raw.len())
        .ok_or_else(|| AssetError::Bundle("truncated: expected a u32".into()))?;
    let value = u32::from_le_bytes(raw[*cursor..end].try_into().unwrap_or([0; 4]));
    *cursor = end;
    Ok(value)
}

/// Reads a little-endian `u64` at `cursor`, advancing it.
fn take_u64(raw: &[u8], cursor: &mut usize) -> Result<u64, AssetError> {
    let end = cursor
        .checked_add(8)
        .filter(|end| *end <= raw.len())
        .ok_or_else(|| AssetError::Bundle("truncated: expected a u64".into()))?;
    let value = u64::from_le_bytes(raw[*cursor..end].try_into().unwrap_or([0; 8]));
    *cursor = end;
    Ok(value)
}

impl Bundle {
    /// Reads and parses the `.pak` file at `path`.
    ///
    /// # Errors
    ///
    /// [`AssetError::Bundle`] if the file can't be read, the magic is
    /// wrong, the index is truncated, or an entry's `offset + length`
    /// runs past the data section.
    pub fn open(path: &Path) -> Result<Self, AssetError> {
        let raw = std::fs::read(path).map_err(|err| io_err(&path.display().to_string(), &err))?;
        Self::parse(raw)
    }

    /// Parses an already-loaded `.pak` byte buffer.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Bundle::open`], minus the file read.
    pub fn parse(raw: Vec<u8>) -> Result<Self, AssetError> {
        if raw.len() < 12 || &raw[..8] != BUNDLE_MAGIC {
            return Err(AssetError::Bundle(
                "not a VGEPAK archive (bad magic)".into(),
            ));
        }
        let mut cursor = 8;
        let count = take_u32(&raw, &mut cursor)? as usize;

        let mut entries = Vec::with_capacity(count.min(4096));
        for _ in 0..count {
            let path_len = take_u32(&raw, &mut cursor)? as usize;
            let path_end = cursor
                .checked_add(path_len)
                .filter(|end| *end <= raw.len())
                .ok_or_else(|| AssetError::Bundle("truncated entry path".into()))?;
            let path = std::str::from_utf8(&raw[cursor..path_end])
                .map_err(|_| AssetError::Bundle("entry path is not UTF-8".into()))?
                .to_string();
            cursor = path_end;
            let offset = take_u64(&raw, &mut cursor)?;
            let length = take_u64(&raw, &mut cursor)?;
            entries.push(Entry {
                path,
                offset,
                length,
            });
        }

        let data_start = cursor;
        let data_len = (raw.len() - data_start) as u64;
        for entry in &entries {
            let end = entry
                .offset
                .checked_add(entry.length)
                .ok_or_else(|| AssetError::Bundle(format!("entry {} overflows", entry.path)))?;
            if end > data_len {
                return Err(AssetError::Bundle(format!(
                    "entry {} runs past the data section",
                    entry.path
                )));
            }
        }

        Ok(Self {
            raw,
            data_start,
            entries,
        })
    }

    /// The bytes for `relative_path` (`/`-separated), or `None` if the
    /// archive has no such entry.
    pub fn get(&self, relative_path: &str) -> Option<&[u8]> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.path == relative_path)?;
        let start = self.data_start + entry.offset as usize;
        let end = start + entry.length as usize;
        self.raw.get(start..end)
    }

    /// Every packed path, in archive order.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.path.as_str())
    }

    /// How many files the archive holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the archive is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "vge-engine_asset-bundle-{name}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn pack_then_open_round_trips_every_file() {
        let src = temp_dir("roundtrip");
        std::fs::create_dir_all(src.join("textures")).unwrap();
        std::fs::write(src.join("scene.ron"), b"(entities: [])").unwrap();
        std::fs::write(src.join("textures/brick.png"), b"\x89PNG fake bytes").unwrap();
        std::fs::write(src.join("empty.bin"), b"").unwrap();

        let pak = src.join("out.pak");
        let stats = pack_dir(&src, &pak).unwrap();
        assert_eq!(stats.files, 3);

        let bundle = Bundle::open(&pak).unwrap();
        assert_eq!(bundle.len(), 3);
        assert_eq!(bundle.get("scene.ron"), Some(&b"(entities: [])"[..]));
        assert_eq!(
            bundle.get("textures/brick.png"),
            Some(&b"\x89PNG fake bytes"[..])
        );
        assert_eq!(bundle.get("empty.bin"), Some(&b""[..]));
        assert_eq!(bundle.get("missing"), None);

        let mut paths: Vec<&str> = bundle.paths().collect();
        paths.sort();
        assert_eq!(paths, vec!["empty.bin", "scene.ron", "textures/brick.png"]);

        std::fs::remove_dir_all(&src).ok();
    }

    #[test]
    fn parse_rejects_a_bad_magic() {
        let err = Bundle::parse(b"NOTVGEPAKxxxx".to_vec()).unwrap_err();
        assert!(matches!(err, AssetError::Bundle(_)));
    }

    #[test]
    fn parse_rejects_a_truncated_index() {
        let mut raw = BUNDLE_MAGIC.to_vec();
        raw.extend_from_slice(&5u32.to_le_bytes()); // claims 5 entries, no data
        assert!(matches!(Bundle::parse(raw), Err(AssetError::Bundle(_))));
    }

    #[test]
    fn parse_rejects_an_entry_past_the_data_section() {
        let mut raw = BUNDLE_MAGIC.to_vec();
        raw.extend_from_slice(&1u32.to_le_bytes()); // 1 entry
        raw.extend_from_slice(&1u32.to_le_bytes()); // path_len = 1
        raw.push(b'a');
        raw.extend_from_slice(&0u64.to_le_bytes()); // offset 0
        raw.extend_from_slice(&99u64.to_le_bytes()); // length 99, but no data
        assert!(matches!(Bundle::parse(raw), Err(AssetError::Bundle(_))));
    }

    #[test]
    fn pack_of_an_empty_dir_makes_a_valid_empty_bundle() {
        let src = temp_dir("empty");
        let pak = src.join("e.pak");
        let stats = pack_dir(&src, &pak).unwrap();
        assert_eq!(stats.files, 0);
        let bundle = Bundle::open(&pak).unwrap();
        assert!(bundle.is_empty());
        std::fs::remove_dir_all(&src).ok();
    }
}
