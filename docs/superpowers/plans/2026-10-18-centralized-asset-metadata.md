# Centralized Asset Metadata Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Store asset metadata under `.studio/assets/` without changing asset identity or breaking loose and packed runtime resolution.

**Architecture:** `engine_asset` introduces `AssetMetadataStore`, the sole owner of metadata path mapping, atomic persistence, and legacy-sidecar migration. Editor/import, runtime, export, and island baking receive the asset and metadata roots explicitly; exported bundles retain assets at their current keys and add metadata below `.studio/assets/`.

**Tech Stack:** Rust 2024 workspace, `uuid`, `ron`, standard filesystem APIs.

**Spec:** `docs/superpowers/specs/2026-10-18-centralized-asset-metadata-design.md`

## Global Constraints

- `.studio/assets/` is versioned project data and must be packed into exports.
- Preserve UUID and `source_hash` when migrating an adjacent legacy sidecar.
- Do not silently generate a replacement ID after a malformed or unreadable record.
- Metadata paths must remain contained by their declared roots; do not follow path escapes.
- Preserve compatibility for old packed bundles with adjacent `.meta` entries.
- Do not stage or modify unrelated worktree changes.

## Review Focus

- Legacy record and central record both exist: central record wins; cleanup must never overwrite it.
- Central write fails: source-side legacy metadata remains intact and no fresh UUID is used.
- Asset path includes nested directories: central path mirrors it exactly under `.studio/assets/`.
- Loose-project runtime: metadata is separate from `assets/`, while glTF relative texture loading still uses `assets/` as its source root.
- Older bundle with `thing.gltf.meta`: runtime continues to index it.

### Task 1: Central metadata store and compatibility migration

**Files:**
- Modify: `crates/engine_asset/src/meta.rs:1-445`
- Modify: `crates/engine_asset/src/lib.rs:1-50`

**Interfaces:**
- Produces: `pub struct AssetMetadataStore`, constructed as `AssetMetadataStore::new(assets_root: impl Into<PathBuf>, metadata_root: impl Into<PathBuf>)`.
- Produces: `metadata_path(&self, asset_path: &Path) -> Result<PathBuf, AssetError>`, `load_or_create`, `refresh`, `ensure`, `confirm`, and `remove` methods that replace direct sidecar persistence.
- Consumes: the existing `AssetMeta`, `hash_bytes`, `AssetError`, and atomic-write helper.

- [ ] **Step 1: Write failing store tests**

Add tests proving a nested new asset creates only `.studio/assets/<relative>.meta`; a valid adjacent legacy record migrates byte-equivalently with the same UUID/hash; a malformed legacy record is an error without a replacement central record; and a central record takes precedence when both files exist.

- [ ] **Step 2: Run the store tests to verify they fail**

Run: `cargo test -p engine_asset metadata_store`

Expected: FAIL because `AssetMetadataStore` does not exist.

- [ ] **Step 3: Implement `AssetMetadataStore` in `meta.rs`**

Derive metadata paths from a source path relative to `assets_root`, rejecting paths outside it. Move `load_or_create`, `refresh`, import-cache persistence, and obsolete-file removal operations behind the store. On first central lookup, read and atomically copy a valid legacy sidecar, read back the central copy, then remove the legacy file; otherwise create a new central record. Keep `AssetMeta` as the RON data type and make its old `sidecar_path` private or remove it after all callers migrate.

- [ ] **Step 4: Run the store tests to verify they pass**

Run: `cargo test -p engine_asset metadata_store`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/engine_asset/src/meta.rs crates/engine_asset/src/lib.rs
git commit -m "feat(asset): centralize asset metadata"
```

### Task 2: Pack and index centralized metadata

**Files:**
- Modify: `crates/engine_asset/src/bundle.rs:44-170`
- Modify: `crates/engine_asset/src/lib.rs:1-50`
- Modify: `crates/engine_scene/src/runtime.rs:1-175, 650-855`

**Interfaces:**
- Produces: `engine_asset::pack_dirs(&[(archive_prefix: &str, source_dir: &Path)], out_path: &Path) -> Result<BundleStats, AssetError>` while retaining `pack_dir` as its single-root convenience wrapper.
- Produces: `AssetLibrary::index(assets_dir: &Path, metadata_dir: &Path) -> Result<Self, SceneError>`.
- Produces: bundled indexing of `.studio/assets/<relative>.meta`, followed by legacy adjacent `.meta` fallback.

- [ ] **Step 1: Write failing bundle/runtime tests**

Add a bundle test that preserves deterministic `assets/...` and `.studio/assets/...` keys from two roots. Add runtime tests that index a loose split-root project and a packed split-root project by UUID; retain/add a test proving a legacy adjacent-meta bundle still loads.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run: `cargo test -p engine_asset pack_dirs && cargo test -p engine_scene asset_library`

Expected: FAIL because multi-root packaging and split-root indexing are absent.

- [ ] **Step 3: Implement multi-root packing and split-root `AssetLibrary` indexing**

Generalize file collection to accept a trusted archive prefix and reject duplicate archive keys. Index central metadata by stripping `.studio/assets/` to obtain the asset-relative key. For loose projects, walk only the metadata root and read assets from the existing assets root. Keep old adjacent bundle entries as a fallback so pre-migration exports remain valid.

- [ ] **Step 4: Run the focused tests to verify they pass**

Run: `cargo test -p engine_asset pack_dirs && cargo test -p engine_scene asset_library`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/engine_asset/src/bundle.rs crates/engine_asset/src/lib.rs crates/engine_scene/src/runtime.rs
git commit -m "feat(scene): index centralized asset metadata"
```

### Task 3: Wire editor, project export, and loose-game callers

**Files:**
- Modify: `crates/engine_editor/src/assets.rs:19, 196-226, 1238-1290`
- Modify: `crates/engine_editor/src/import.rs:112-225, tests`
- Modify: `editor/src/main.rs:470-675, tests`
- Modify: `crates/engine_project/src/build.rs:64-270, tests`
- Modify: `apps/player/src/main.rs:32-65, tests`
- Modify: `games/island/src/main.rs:448-464`

**Interfaces:**
- Consumes: `AssetMetadataStore`, revised `ImportCache`, `pack_dirs`, and split-root `AssetLibrary::index`.
- Produces: `ExportPlan::metadata_src: PathBuf`, populated as `project.studio_dir().join("assets")`.

- [ ] **Step 1: Write failing integration-boundary tests**

Update editor importer/asset-index tests to assert central metadata paths and legacy preservation. Update export-plan tests to assert its metadata source, and player loose-directory tests to use `.studio/assets/thing.gltf.meta` while preserving the old packed fallback test. Add an export test that opens the produced bundle and resolves a central-metadata UUID.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run: `cargo test -p engine_editor && cargo test -p engine_project && cargo test -p player`

Expected: FAIL at callers still using adjacent paths or one-root export packing.

- [ ] **Step 3: Pass both roots at every project boundary**

Construct the store from `Project::assets_dir()` and `Project::studio_dir().join("assets")`; pass it through editor ID resolution and importer cache operations. Filter watcher events for legacy `.meta` files only as a compatibility measure. Have export package `assets/` at `assets` keys and the metadata root at `.studio/assets` keys. Update all loose `AssetLibrary::index` callers to pass both roots.

- [ ] **Step 4: Run the focused tests to verify they pass**

Run: `cargo test -p engine_editor && cargo test -p engine_project && cargo test -p player`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/engine_editor/src/assets.rs crates/engine_editor/src/import.rs editor/src/main.rs crates/engine_project/src/build.rs apps/player/src/main.rs games/island/src/main.rs
git commit -m "feat(project): export centralized asset metadata"
```

### Task 4: Update island bake and migrate sample metadata

**Files:**
- Modify: `games/island/src/bake.rs:1-35, 603-669, tests`
- Move: `games/island/assets/**/*.meta` to `games/island/.studio/assets/**/*.meta`

**Interfaces:**
- Consumes: `AssetMetadataStore` for baked asset writes, ID refresh, and deleting obsolete model records.

- [ ] **Step 1: Write failing island regression tests**

Update bake tests to assert IDs are stable across two bakes using the central metadata root, no adjacent metadata file exists for a new bake, and obsolete generated models remove their matching central metadata record.

- [ ] **Step 2: Run the bake tests to verify they fail**

Run: `cargo test -p island bake`

Expected: FAIL because baking still writes/cleans adjacent sidecars.

- [ ] **Step 3: Implement centralized bake metadata and move tracked sample records**

Pass the project asset and metadata roots into bake helpers rather than deriving sidecars from individual files. Move each existing sample `.meta` file without changing content, preserving scene IDs. Do not edit non-metadata assets or unrelated current worktree changes.

- [ ] **Step 4: Run the bake tests to verify they pass**

Run: `cargo test -p island bake`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add games/island/src/bake.rs games/island/.studio/assets
git rm --cached -- games/island/assets/**/*.meta
git commit -m "chore(island): move asset metadata to studio"
```

### Task 5: Full verification and compatibility review

**Files:**
- Modify only if preceding checks reveal a task-scoped defect.

- [ ] **Step 1: Run formatting**

Run: `cargo fmt --all -- --check`

Expected: PASS.

- [ ] **Step 2: Run linting**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings`

Expected: PASS.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test --workspace --all-features`

Expected: PASS.

- [ ] **Step 4: Review the final diff**

Confirm no unrelated user edits are staged, all sample metadata contents and UUIDs are preserved, no new adjacent `.meta` files are created, and exports include `.studio/assets` metadata entries.

