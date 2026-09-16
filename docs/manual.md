# RustyEngine Studio User Manual

## Requirements

Install stable Rust and Cargo. The editor is currently developed and
validated on macOS; Linux and Windows builds are checked in CI.

## Open the editor

From the repository root:

```sh
cargo run -p editor
```

Pass a project directory to open it directly:

```sh
cargo run -p editor -- path/to/project
```

The editor opens in a maximized window (not macOS full-screen mode).
Window dimensions are specified in logical pixels, so the same minimum
usable area is preserved when moving the window between monitors with
different display scaling.

## Create or open a project

Use **File → New Project** or **File → Open Project**. Projects contain a
`project.ron`, a `scene.ron`, an `assets/` directory, and a Rust `src/`
tree. The editor restores the last scene and panel layout from `.studio/`.

## Scene editing

Select entities in the Hierarchy. The Inspector edits names, transforms,
cameras, sprites, colliders, audio emitters, asset sources, and scripts.
The Scene View gizmo supports translate, rotate, and scale. `Cmd/Ctrl-Z`
undoes a completed edit; `Cmd/Ctrl-Shift-Z` or `Cmd/Ctrl-Y` redoes it.

## Assets

Drop files or folders from Finder into the Asset Browser. Supported imports
include glTF/GLB meshes, PNG/JPEG textures, and WAV audio. Rows can be
selected or dragged into the Scene View. The preview panel is controlled by
**Window → Asset Preview**.

## Play and build

Press **Play** to save the edited scene, build the selected configuration,
and launch the project in a separate managed process. **Stop** terminates
that process and restores the editor's pre-play scene. Build and process
output appears in the Output panel.

## Terrain and vegetation

Open **Terrain & Vegetation Tools** in the Scene View for raise, lower,
smooth, flatten, and deterministic scatter controls. Terrain assets use
validated RON data and are written atomically.

## Troubleshooting

- If an asset is missing, the Inspector keeps its stable reference and the
  Asset Browser reports unresolved dependencies.
- If an import fails, the last valid imported resource remains active.
- If the editor crashes, inspect `.studio/crash.log` in the project root.
- If audio tests cannot access a CoreAudio device, backend-dependent tests
  skip cleanly; this does not indicate a scene error.

## Current limitations

Provider transport, public-distribution signing, and full GPU timestamp
collection remain optional. The editor now includes the provider-neutral AI
tool/session layers and production docking shell, but no AI network provider
or background updater is enabled by default. Release packaging is available
through CI artifacts; local macOS use does not require signing.

## Updates and localization

The editor currently ships an English catalog and exposes stable localization
keys for future translations. There is no background updater or network call.
To update safely, download the matching CI release archive, quit the editor,
replace the application binary, and reopen the project. Project data remains
in the project directory; keep a backup before changing versions.
