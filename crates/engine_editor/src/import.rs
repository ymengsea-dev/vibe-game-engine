//! The editor's asset import pass.
//!
//! [`AssetImporter::run`] walks the referenceable asset entries (mesh /
//! texture / audio — see [`AssetKind::is_referenceable`]), runs the
//! matching `engine_asset` importer for each, and holds the decoded
//! result in memory keyed by its stable [`AssetId`]. Every pass is gated
//! by an [`engine_asset::ImportCache`]: a source file whose bytes are
//! unchanged since the last pass is **not** decoded again — its record
//! reads [`ImportOutcome::Cached`] and the previously imported data is
//! kept.
//!
//! This also builds the id ↔ path [`AssetIndex`] (the same one
//! [`crate::resolve_asset_ids`] produces on its own), so the shell runs
//! one pass at startup instead of two.
//!
//! ## Not done here
//!
//! - No GPU upload and no viewport rendering of imported meshes/textures
//!   — the imported data is in-memory bookkeeping only (that wiring is
//!   the Runtime & build track).
//! - No background threading — importers run synchronously on the
//!   startup path; an [`engine_asset::AssetLoader`] integration is later.
//! - No per-row status badges in the browser — only a one-line summary
//!   (see [`AssetImporter::stats`]).
//! - A `.gltf` that references external `.bin`/image files fails
//!   (`engine_asset::import_gltf_slice` takes a self-contained byte
//!   slice); it is recorded as [`ImportOutcome::Failed`], not fatal.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use engine_asset::{
    AssetId, ImportCache, ImportedAudio, ImportedGltf, ImportedTexture, import_gltf_slice,
    import_texture_bytes, import_wav_bytes,
};

use crate::assets::{AssetEntry, AssetIndex, AssetKind};

/// What happened to one asset in an [`AssetImporter::run`] pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// The importer ran this pass (first sight, or the source changed).
    Imported,
    /// The source was unchanged and already imported — importer skipped.
    Cached,
    /// Reading or decoding the source failed; the message is the error.
    Failed(String),
}

/// One imported asset, held in memory by [`AssetImporter`].
///
/// Boxed per variant so the enum stays pointer-sized regardless of how
/// large the individual imported types are.
#[derive(Debug)]
pub enum ImportedAsset {
    /// A `.gltf`/`.glb` import (`engine_asset::import_gltf_slice`).
    Mesh(Box<ImportedGltf>),
    /// A `.png`/`.jpg`/`.jpeg` import (`engine_asset::import_texture_bytes`).
    Texture(Box<ImportedTexture>),
    /// A `.wav` import (`engine_asset::import_wav_bytes`).
    Audio(Box<ImportedAudio>),
}

/// The result of importing (or skipping) one asset file in a pass.
#[derive(Debug, Clone)]
pub struct ImportRecord {
    /// The asset's stable id, or `None` if the `.meta` sidecar itself
    /// could not be read/written.
    pub id: Option<AssetId>,
    /// Path relative to the assets root.
    pub relative_path: std::path::PathBuf,
    /// The kind that selected the importer.
    pub kind: AssetKind,
    /// What happened.
    pub outcome: ImportOutcome,
}

/// A tally of the last [`AssetImporter::run`] pass, for a status line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportStats {
    /// Referenceable assets seen this pass.
    pub total: usize,
    /// How many the importer actually ran for.
    pub imported: usize,
    /// How many were served from the cache unchanged.
    pub cached: usize,
    /// How many failed to read or decode.
    pub failed: usize,
}

/// Owns the [`ImportCache`], the in-memory imported-data map, and the
/// records from the most recent [`AssetImporter::run`].
#[derive(Debug, Default)]
pub struct AssetImporter {
    cache: ImportCache,
    imported: HashMap<AssetId, ImportedAsset>,
    records: Vec<ImportRecord>,
    pass_id: u64,
}

impl AssetImporter {
    /// An importer with an empty cache and nothing imported yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Imports every [`AssetKind::is_referenceable`] entry under `root`,
    /// skipping any whose source is byte-for-byte unchanged since the
    /// last pass, and returns the id ↔ path index.
    ///
    /// Side effects: each referenceable entry gets a `.meta` sidecar (if
    /// absent) and its [`AssetEntry::id`] filled in; imported data is
    /// stored on `self`; [`AssetImporter::records`] is replaced with this
    /// pass's outcomes. Non-referenceable entries (scenes, loose files)
    /// are ignored entirely.
    ///
    /// Never returns an error: a per-file failure (missing bytes, bad
    /// `.meta`, undecodable source) is logged once and recorded as
    /// [`ImportOutcome::Failed`] so one broken asset can't stop the rest.
    ///
    /// In-memory data for an asset no longer present in `entries` (its
    /// source file was deleted between passes) is dropped — so a
    /// re-`run` after a hot-reload change also acts as eviction.
    pub fn run(&mut self, root: &Path, entries: &mut [AssetEntry]) -> AssetIndex {
        let mut index = AssetIndex::new();
        self.records.clear();
        self.pass_id = self.pass_id.wrapping_add(1);
        let mut seen: HashSet<AssetId> = HashSet::new();

        for entry in entries.iter_mut() {
            if !entry.kind.is_referenceable() {
                continue;
            }
            let absolute = root.join(&entry.relative_path);

            let (id, source_changed) = match self.cache.ensure(&absolute) {
                Ok((uuid, needs_import)) => (AssetId::from_uuid(uuid), needs_import),
                Err(err) => {
                    tracing::warn!(
                        path = %absolute.display(), error = %err,
                        "could not read or create the asset .meta sidecar"
                    );
                    self.records.push(ImportRecord {
                        id: None,
                        relative_path: entry.relative_path.clone(),
                        kind: entry.kind,
                        outcome: ImportOutcome::Failed(err.to_string()),
                    });
                    continue;
                }
            };

            entry.id = Some(id);
            seen.insert(id);
            index.insert(id, entry.relative_path.clone());

            // Re-import if the bytes changed, or if we have a warm `.meta`
            // from an earlier run but no decoded data in memory yet.
            let must_import = source_changed || !self.imported.contains_key(&id);
            let outcome = if must_import {
                match import_one(entry.kind, &absolute) {
                    Ok(asset) => {
                        self.imported.insert(id, asset);
                        ImportOutcome::Imported
                    }
                    Err(message) => {
                        tracing::warn!(
                            path = %absolute.display(), error = %message,
                            "asset import failed"
                        );
                        ImportOutcome::Failed(message)
                    }
                }
            } else {
                ImportOutcome::Cached
            };

            self.records.push(ImportRecord {
                id: Some(id),
                relative_path: entry.relative_path.clone(),
                kind: entry.kind,
                outcome,
            });
        }

        self.imported.retain(|id, _| seen.contains(id));
        index
    }

    /// The imported data for `id`, if a pass has imported it and it did
    /// not later fail.
    pub fn get(&self, id: AssetId) -> Option<&ImportedAsset> {
        self.imported.get(&id)
    }

    /// The per-asset outcomes of the most recent [`AssetImporter::run`].
    pub fn records(&self) -> &[ImportRecord] {
        &self.records
    }

    /// How many assets are currently held in memory.
    pub fn imported_count(&self) -> usize {
        self.imported.len()
    }

    /// A counter bumped once per [`AssetImporter::run`] — a cheap key for
    /// caches (e.g. asset previews) that must invalidate after a
    /// re-import.
    pub fn pass_id(&self) -> u64 {
        self.pass_id
    }

    /// A tally of the most recent pass, for the browser's status line.
    pub fn stats(&self) -> ImportStats {
        let mut stats = ImportStats {
            total: self.records.len(),
            ..ImportStats::default()
        };
        for record in &self.records {
            match record.outcome {
                ImportOutcome::Imported => stats.imported += 1,
                ImportOutcome::Cached => stats.cached += 1,
                ImportOutcome::Failed(_) => stats.failed += 1,
            }
        }
        stats
    }
}

/// Reads `absolute` and runs the importer for `kind`. `kind` is always
/// one of the [`AssetKind::is_referenceable`] variants — `run` filters
/// the rest out before calling this.
fn import_one(kind: AssetKind, absolute: &Path) -> Result<ImportedAsset, String> {
    let bytes = fs::read(absolute).map_err(|err| err.to_string())?;
    match kind {
        AssetKind::Mesh => import_gltf_slice(&bytes)
            .map(|gltf| ImportedAsset::Mesh(Box::new(gltf)))
            .map_err(|err| err.to_string()),
        AssetKind::Texture => import_texture_bytes(&bytes)
            .map(|texture| ImportedAsset::Texture(Box::new(texture)))
            .map_err(|err| err.to_string()),
        AssetKind::Audio => import_wav_bytes(&bytes)
            .map(|audio| ImportedAsset::Audio(Box::new(audio)))
            .map_err(|err| err.to_string()),
        AssetKind::Scene | AssetKind::Prefab | AssetKind::Other => {
            Err("no importer for this asset kind".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "vge-engine_editor-import-test-{name}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_png(path: &Path, width: u32, height: u32) {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 20, 30, 255]));
        image
            .save_with_format(path, image::ImageFormat::Png)
            .unwrap();
    }

    /// A minimal 16-bit mono PCM WAV, 3 samples.
    fn write_wav(path: &Path) {
        let samples: [i16; 3] = [0, 1_000, -1_000];
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&8_000u32.to_le_bytes()); // sample rate
        out.extend_from_slice(&16_000u32.to_le_bytes()); // byte rate
        out.extend_from_slice(&2u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        std::fs::write(path, out).unwrap();
    }

    fn scan(dir: &Path) -> Vec<AssetEntry> {
        crate::assets::scan(dir).unwrap()
    }

    fn outcome_of<'a>(importer: &'a AssetImporter, name: &str) -> &'a ImportOutcome {
        &importer
            .records()
            .iter()
            .find(|record| record.relative_path == Path::new(name))
            .expect("a record for the named asset")
            .outcome
    }

    #[test]
    fn run_imports_referenceable_kinds_and_holds_the_data() {
        let dir = temp_dir("imports");
        write_png(&dir.join("brick.png"), 4, 4);
        write_wav(&dir.join("hit.wav"));
        std::fs::write(dir.join("notes.txt"), b"text").unwrap();
        std::fs::write(dir.join("main.ron"), b"()").unwrap();

        let mut entries = scan(&dir);
        let mut importer = AssetImporter::new();
        let index = importer.run(&dir, &mut entries);

        assert_eq!(
            importer.records().len(),
            2,
            "only the png and wav produce records"
        );
        assert!(
            importer
                .records()
                .iter()
                .all(|record| record.outcome == ImportOutcome::Imported)
        );

        let png_id = entries
            .iter()
            .find(|entry| entry.relative_path == Path::new("brick.png"))
            .unwrap()
            .id
            .unwrap();
        assert!(matches!(
            importer.get(png_id),
            Some(ImportedAsset::Texture(_))
        ));
        assert!(index.id_for(Path::new("hit.wav")).is_some());
    }

    #[test]
    fn second_run_reports_cached_and_skips_the_importer() {
        let dir = temp_dir("cached");
        write_png(&dir.join("brick.png"), 2, 2);
        let mut entries = scan(&dir);
        let mut importer = AssetImporter::new();

        importer.run(&dir, &mut entries);
        assert_eq!(importer.stats().imported, 1);

        importer.run(&dir, &mut entries);
        let stats = importer.stats();
        assert_eq!(stats.imported, 0, "unchanged source is not re-decoded");
        assert_eq!(stats.cached, 1);
        assert_eq!(importer.imported_count(), 1);
    }

    #[test]
    fn editing_a_source_triggers_reimport() {
        let dir = temp_dir("edit");
        write_png(&dir.join("a.png"), 2, 2);
        write_wav(&dir.join("b.wav"));
        let mut entries = scan(&dir);
        let mut importer = AssetImporter::new();
        importer.run(&dir, &mut entries);

        write_png(&dir.join("a.png"), 8, 8); // different bytes
        importer.run(&dir, &mut entries);

        assert_eq!(outcome_of(&importer, "a.png"), &ImportOutcome::Imported);
        assert_eq!(outcome_of(&importer, "b.wav"), &ImportOutcome::Cached);
    }

    #[test]
    fn a_corrupt_source_is_failed_not_fatal() {
        let dir = temp_dir("corrupt");
        std::fs::write(dir.join("brick.png"), b"not a png").unwrap();
        write_wav(&dir.join("hit.wav"));
        let mut entries = scan(&dir);
        let mut importer = AssetImporter::new();
        importer.run(&dir, &mut entries);

        assert!(matches!(
            outcome_of(&importer, "brick.png"),
            ImportOutcome::Failed(_)
        ));
        assert_eq!(
            outcome_of(&importer, "hit.wav"),
            &ImportOutcome::Imported,
            "a sibling still imports"
        );
    }

    #[test]
    fn cold_start_with_a_warm_meta_still_imports_once() {
        let dir = temp_dir("warm-meta");
        write_png(&dir.join("brick.png"), 2, 2);
        // A prior run already wrote a matching `.meta` sidecar.
        engine_asset::AssetMeta::load_or_create(&dir.join("brick.png")).unwrap();

        let mut entries = scan(&dir);
        let mut importer = AssetImporter::new();
        importer.run(&dir, &mut entries);

        assert_eq!(
            importer.records()[0].outcome,
            ImportOutcome::Imported,
            "no in-memory data yet, so it imports despite the fresh meta"
        );
        assert_eq!(importer.imported_count(), 1);
    }

    #[test]
    fn run_builds_a_two_way_asset_index() {
        let dir = temp_dir("index");
        write_png(&dir.join("brick.png"), 2, 2);
        let mut entries = scan(&dir);
        let mut importer = AssetImporter::new();
        let index = importer.run(&dir, &mut entries);

        let id = entries[0].id.unwrap();
        assert_eq!(index.path_for(id), Some(Path::new("brick.png")));
        assert_eq!(index.id_for(Path::new("brick.png")), Some(id));
    }

    #[test]
    fn stats_counts_each_outcome() {
        let dir = temp_dir("stats");
        write_png(&dir.join("ok.png"), 2, 2);
        std::fs::write(dir.join("bad.png"), b"nope").unwrap();
        let mut entries = scan(&dir);
        let mut importer = AssetImporter::new();
        importer.run(&dir, &mut entries);

        let stats = importer.stats();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.imported, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.cached, 0);
    }

    #[test]
    fn run_evicts_data_for_a_removed_asset() {
        let dir = temp_dir("evict");
        write_png(&dir.join("keep.png"), 2, 2);
        write_wav(&dir.join("drop.wav"));

        let mut entries = scan(&dir);
        let mut importer = AssetImporter::new();
        importer.run(&dir, &mut entries);
        assert_eq!(importer.imported_count(), 2);
        let dropped_id = entries
            .iter()
            .find(|entry| entry.relative_path == Path::new("drop.wav"))
            .unwrap()
            .id
            .unwrap();

        // The source vanishes; a rescan no longer lists it.
        std::fs::remove_file(dir.join("drop.wav")).unwrap();
        let mut entries = scan(&dir);
        importer.run(&dir, &mut entries);

        assert_eq!(importer.imported_count(), 1);
        assert!(
            importer.get(dropped_id).is_none(),
            "removed asset is evicted"
        );
        let kept_id = entries
            .iter()
            .find(|entry| entry.relative_path == Path::new("keep.png"))
            .unwrap()
            .id
            .unwrap();
        assert!(
            importer.get(kept_id).is_some(),
            "surviving asset is retained"
        );
    }
}
