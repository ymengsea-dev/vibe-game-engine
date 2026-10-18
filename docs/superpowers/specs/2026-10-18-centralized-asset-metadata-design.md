# Centralized Asset Metadata Design

## Goal

Keep user-authored files under `assets/` free of metadata sidecars while
preserving stable asset IDs, incremental imports, scene resolution, loose
runtime loading, packed exports, and existing projects.

## Scope

This changes only the storage location of `AssetMeta` records. It does not
add import settings, change scene syntax, introduce a database, or alter
asset formats.

## On-disk layout

For an asset relative to the project's `assets/` directory, metadata lives
at the matching path beneath `.studio/assets/`:

```text
assets/textures/grass.png
.studio/assets/textures/grass.png.meta
```

`.studio/assets/` is project data: it is committed with the project and
carried into exports. Other `.studio/` state remains editor-only. The path
mirroring keeps metadata independent per asset and makes it easy to inspect,
move, merge, and clean up without a shared registry file.

## Ownership and API

`engine_asset` owns metadata path derivation, persistence, hashing, and
legacy-sidecar migration. It gains a small metadata-store value constructed
from the asset root and metadata root. All metadata reads and writes use this
store; callers no longer derive a `.meta` path directly.

The store only accepts paths contained by the asset root. A malformed
metadata file remains a typed error; an inaccessible metadata directory does
not silently create a new ID. Existing import failure reporting is retained.

`engine_project::Project` supplies the two roots through its existing
`assets_dir()` and `studio_dir()` accessors. Components that operate without
a `Project` receive both roots explicitly.

## Compatibility migration

When a central metadata record is absent, the store checks the legacy
adjacent `<asset>.meta` file. If it parses, it atomically writes the exact
record to `.studio/assets/<asset>.meta`, verifies the new file can be read
back, then removes the legacy file. This preserves its UUID and last imported
hash. The operation is retry-safe:

- a central record always wins, so an interrupted cleanup cannot change IDs;
- a failed central write leaves the legacy file untouched;
- a failed legacy removal leaves two equivalent records, and a later pass
  removes only the legacy copy;
- a malformed legacy record is reported as before and is never replaced with
  a fresh ID.

New metadata is written only to `.studio/assets/`. Renames are supported by
moving the matching metadata path with the asset when the editor owns the
move; externally moved files retain the existing behavior of receiving a new
ID on their next import. The migration never scans or deletes arbitrary
files; it considers only the exact sidecar corresponding to a known asset.

## Consumers

The editor asset scan/import cache and island baker use the store for ID
creation, refresh, and obsolete-asset cleanup. Their asset browser no longer
needs sidecar filtering for current projects, but continues ignoring legacy
sidecars while they are migrated.

`AssetLibrary` indexes loose projects from `assets/` plus `.studio/assets/`.
Packed exports contain normal asset entries and metadata entries under
`.studio/assets/`; `AssetLibrary::from_bundle` indexes that prefix and maps
each metadata record back to its asset-relative path. This keeps the shipping
asset archive self-contained without exposing metadata alongside source files.

`ExportPlan` carries the metadata source directory and asks the asset bundle
writer to package the two roots with their defined archive prefixes. Existing
exports created by older versions remain readable through the legacy
adjacent-sidecar scan in `AssetLibrary::from_bundle`.

## Error handling

The importer and runtime retain their current partial-failure stance: one
unreadable or malformed record is logged/skipped without stopping unrelated
assets. Export fails rather than producing a bundle that silently omits the
metadata root. Path containment is checked before reading, writing, moving,
or packaging metadata.

## Acceptance criteria

- Creating/importing an asset creates only `.studio/assets/<relative>.meta`.
- A legacy sidecar migrates without changing its UUID or source hash, then is
  removed only after the central record is durable.
- A failed migration preserves the usable legacy sidecar.
- Loose-project and packed-export runtimes resolve the same scene UUIDs.
- Bundles created before the change remain loadable.
- The island bake path preserves IDs and removes obsolete central metadata.
- No metadata files remain in a newly created project's visible `assets/`
  tree.

## Verification

Add focused unit tests at the metadata store, bundle, runtime library,
editor/importer, project export, and island bake boundaries. Run their crate
tests, then the workspace formatting, Clippy, and workspace test commands
when the implementation is complete.
