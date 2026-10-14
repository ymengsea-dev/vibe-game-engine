# RustyEngine Feature Reference

This document describes the implementation present in the repository as of 2026-10-13. It is a code-verified feature inventory, not a roadmap. “Implemented” means working code and tests exist; “partial” means the core is present but an integration or production capability is still missing; “stub” means the crate declares intent without an implementation.

## Product and Architecture

RustyEngine is a Rust 2024 workspace containing a reusable 2D/3D runtime, an egui-based Studio editor, a standalone exported-game player, examples, and the Island vertical slice. Games normally depend only on the `engine` facade and import `engine::prelude::*`. Editor-only code is behind the facade's `studio` feature; `apps/player` and `games/island` disable default features so shipped runtimes do not link editor UI or editor AI code.

The standard runtime is `engine::app::run_game`. A game implements `Game::setup`, variable-rate `update`, fixed-rate `fixed_update`, and optional event/save hooks. The loop owns the window, GPU, ECS world, physics, audio, input, rendering resources, retained UI, timing, and save policy. Within a frame it polls devices, performs fixed physics steps, processes UI, calls gameplay, updates animation and hierarchy transforms, synchronizes spatial audio, renders, and performs requested or scheduled saves. See `crates/engine/src/app.rs` and `crates/engine/src/lib.rs`.

## Runtime and Platform

### Core application services — implemented

- `engine_core` provides validated engine configuration, application lifecycle states, opt-in structured logging, and a fixed-timestep accumulator with bounded catch-up.
- `engine_platform` owns a single winit window/event loop and converts raw events into engine-owned events. Keyboard, mouse, cursor, wheel, and gamepad state expose held/pressed/released edges.
- `ActionMap` maps named actions to multiple keyboard, mouse, gamepad-button, stick, or key-pair bindings. Radial/axis deadzones are configurable.
- `engine_utils` supplies transforms, stable UUID asset identities, reference-counted typed asset stores, deterministic random generation, and an owned Rayon job system.

Primary sources: `crates/engine_core/src/`, `crates/engine_platform/src/`, and `crates/engine_utils/src/`.

### ECS and world integration — implemented

`engine_ecs` wraps Bevy ECS with named update/render-extract schedules and components for local/global transforms, cameras, meshes, skinned and instanced meshes, vegetation, sprites, names, scripts, asset sources, physics handles, audio, particles, and editor markers. It supports:

- Parent/child relationships with propagated world transforms.
- Physics-to-transform synchronization.
- Asset-handle resolution, frustum culling, render statistics, and explicit handle cleanup on despawn.
- Batched sprite extraction and opaque/transparent mesh ordering.
- Per-entity skeletal animation with shared clips, GPU matrix uploads, looping/one-shot playback, and authored events.
- Transform-driven listeners and spatial emitters, including autoplay and mixer-bus routing.
- Deterministic CPU particle emitters with alpha/additive extraction.

Primary sources: `crates/engine_ecs/src/components.rs`, `render.rs`, `animation.rs`, `audio.rs`, `particles.rs`, and `propagate.rs`.

## Rendering

`engine_renderer` is a wgpu renderer with these implemented paths:

- Perspective and orthographic cameras, view frusta, conservative AABB culling, and a smoothed third-person orbit/follow rig.
- Indexed meshes, dynamic vertex updates, GPU instancing, GPU skeletal skinning (up to 64 joints), and bounded LOD/streaming selection helpers.
- Metallic-roughness PBR with base-color, normal, occlusion, and emissive maps; ambient, directional, and point lights; and alpha modes.
- A shadow map for the first directional light with 3×3 PCF, camera-following shadow fit, and a procedural gradient/sun sky.
- HDR rendering, adapter-dependent 4× MSAA, ACES tonemapping, bloom, color grading, and luminance-based toon outlines.
- A dependency-ordered render graph for shadow, scene, and post-processing passes.
- Atlas-based batched sprites, screen-space UI quads, and a built-in 5×7 bitmap glyph atlas.
- CPU-simulated/GPU-instanced billboard particles with alpha or additive blending.
- Editable heightmap terrain with raise/lower, smooth, and flatten brushes.
- Deterministic terrain-aware scattering and instanced wind-bent vegetation.
- Immediate debug lines for boxes, spheres, capsules, frusta, grids, axes, arrows, and other overlays.

Current limits: only the first directional light casts shadows; there are no point-light shadows or HDRI cubemaps. Toon outlines use scene luminance rather than depth/normals. Particles are untextured and CPU simulated. Runtime UI images currently render as tinted rectangles until an image asset registry is connected.

Primary sources: `crates/engine_renderer/src/` and `crates/engine_renderer/src/shaders/`.

## Assets, Scenes, and Persistence

### Asset pipeline — implemented

`engine_asset` provides stable `.meta` UUIDs, source hashing, import caching, dependency/ref-count bookkeeping, polling-based hot reload, and non-blocking Tokio load handles. Importers support:

- glTF/GLB meshes, PBR materials, embedded or external textures, cameras, skins, joint weights, animation channels, and animation events stored in glTF extras.
- PNG/JPEG and other `image`-supported textures with CPU mip generation.
- PCM 8/16/24/32-bit and 32-bit float WAV audio.
- glTF export for static/skinned meshes, materials, embedded textures, skeletons, animations, and events.
- Self-contained `.pak` asset bundles with bounds-checked parsing.

Compressed audio import is not implemented. See `crates/engine_asset/src/`.

### Scene system — implemented

Scene format v4 is validated, versioned RON. An entity may store a name, hierarchy parent, transform, camera, mesh/material references, asset path/id, sprite, disabled/static/locked flags, collider, and audio emitter. A scene may also contain a level-wide navigation grid. Parsing validates finite transforms, projection ranges, asset IDs, hierarchy bounds/cycles, collider data, and nav-grid shape; additive migrations upgrade older versions.

Scenes can be captured from or instantiated into a live world. `SceneResolver` keeps file format concerns separate from asset decoding/GPU upload; editor and player resolvers cache shared uploads and preserve unresolved references. The runtime asset library reads loose files or bundles. Prefabs serialize a single entity and can be instantiated repeatedly or at an offset.

World snapshots and generic `SaveGame<T>` files use atomic writes and include unnamed entities plus game-owned data. They intentionally omit animation time, physics velocity/body reconstruction, and audio playback position, making them suitable for checkpoints and exit saves rather than exact mid-frame restoration.

Primary sources: `crates/engine_scene/src/format.rs`, `validate.rs`, `world.rs`, `runtime.rs`, `prefab.rs`, and `save.rs`.

## Simulation and Gameplay Systems

### Physics — implemented

`engine_physics` wraps Rapier 3D with rigid bodies, colliders, gravity, fixed-step simulation, raycasts, debug geometry, and a slope/step-aware character controller. The facade can create bodies and build scene-authored ball, cuboid, cylinder, capsule, and triangle-mesh colliders. A separate pure 2D layer provides AABB/circle intersection tests and an axis-separated platformer controller; it is not a Rapier 2D world.

### Animation — implemented, with integration limits

`engine_animation` samples imported translation/rotation/scale channels, computes hierarchy-aware skinning matrices, blends poses, loops clips, evaluates 1D blend trees, and runs boolean-condition state machines with non-interruptible crossfades. State machines currently support one clip per state, boolean parameters only, and no exit-time transitions. ECS animation players are integrated with GPU skinning; state machines remain driven by game code.

### Audio — implemented

`engine_audio` wraps Kira for decoded static sounds, streamed files, looping, master volume, mixer buses, spatial listeners/tracks, and pose updates. Runtime startup degrades to silent operation if no audio backend is available. Scene emitters can autoplay, loop, set gain/radius, and follow entity transforms.

### Runtime UI — implemented, intentionally small

`engine_ui` is a retained, dependency-free node tree rather than egui. It supports fixed-size anchor/offset layout, nested panels, labels, buttons, images, pointer hit testing, click capture, focus rings, wrapping next/previous focus, geometric directional focus, and keyboard/gamepad activation. The engine renders fills and bitmap text in one atlas-backed pass. Scrolling and flex/grid layout are not implemented.

### Gameplay navigation — implemented

`engine_ai` provides a bounded XZ grid, cell/AABB/circle blockers, world/grid conversion, eight-way A* with octile cost and no corner cutting, line-of-sight checks, string-pulled path smoothing, and a pure path-follow step. Polygon navmeshes, local avoidance, ECS agents, and automatic baking from colliders or terrain do not exist yet.

Primary sources: `crates/engine_physics/src/`, `crates/engine_animation/src/`, `crates/engine_audio/src/`, `crates/engine_ui/src/lib.rs`, and `crates/engine_ai/src/nav.rs`.

## Project and Build Workflow

`engine_project` defines a versioned project with `project.ron`, Rust sources, assets, scenes, `.studio` metadata, and named debug/release configurations. It can create, open, migrate, validate, and safely resolve project-relative paths. Export builds the selected crate, copies the platform executable, packs assets into `assets.pak`, and stages the configured scene as `main.ron`. Export targets the host OS; cross-compilation is not implemented.

`apps/player` loads an exported `main.ron`, prefers `assets.pak`, falls back to loose assets, instantiates through the shared runtime resolver, creates scene colliders, adopts a scene camera, and continues past individual unresolved assets. Corrupt or absent scenes produce a clean non-zero exit. The player links no editor crates.

Primary sources: `crates/engine_project/src/` and `apps/player/src/main.rs`.

## RustyEngine Studio

The `editor` binary combines `engine_editor`, `editor_code`, and `editor_lsp` into one native window.

### Project and scene authoring — implemented

- Create, open, and switch projects; retain a bounded recent-project list.
- Persist workspace, panel visibility, selected build configuration, overlays, asset view, preview state, and last scene under `.studio/session.ron`.
- Create/open/save scenes, autosave dirty scenes, detect newer recovery data, and log bounded local crash reports.
- Track scene/settings/document dirtiness and guard destructive close/open/new actions.
- Snapshot-based undo/redo that restores the world and reselects entities by name.
- Hierarchy tree with single/range/toggle selection, duplicate, delete, drag reparenting, locking, static/disabled markers, and descendant-cycle prevention.
- Inspector registry for names, transforms, cameras, sprites, colliders, audio emitters, source assets, and scripts.
- `engine_editor_derive` supplies `#[derive(Inspectable)]`, generating labelled egui field editors for supported component data.
- Translate/rotate/scale gizmos in local or world space.
- A Scene viewport using the same ECS extraction, PBR renderer, asset resolver, and post-processing path as games. Missing geometry is shown with a wireframe placeholder.
- Optional overlays for colliders, lights, cameras, grid, audio, and navigation.
- Single-entity prefab capture/instantiate. Prefab inheritance, child capture, and override tracking are not implemented.

### Assets and environment authoring — implemented

- Folder/list asset browsing with stable ID resolution and missing-dependency reporting.
- File/folder drag-and-drop and native import. Assets are copied into the project and renamed on collision rather than overwritten.
- Synchronous cached import, polling hot reload, and preservation of the last valid import after a failed reload.
- Texture, mesh, and audio summaries/previews, including bounded thumbnails and waveform bins.
- Dragging assets into the scene and resolving imported CPU data into cached GPU resources.
- Terrain brush state and validated atomic terrain files, plus deterministic vegetation scatter preview controls.

### Code and diagnostics — implemented

- Offline vendored Monaco in a child webview, with file tree, dirty-buffer events, cursor tracking, save requests, diagnostic markers, and source reveal.
- A non-blocking rust-analyzer process supporting completion, hover, definition, references, signature help, rename edits, workspace symbols, and published diagnostics.
- Project-wide literal text search plus rust-analyzer symbol search.
- Console capture from `tracing`, Problems diagnostics, bounded build/runtime Output, and a real feature-gated PTY terminal.
- A rolling CPU frame profiler with average, peak, FPS estimate, sparkline, and named spans. GPU timestamp profiling is not implemented.

### Play, export, and shell — implemented with feature gates

Play saves the scene, builds the selected project, launches it as a managed child process, streams output, supports stop, and restores the pre-play editor scene. Export runs in the background and reports staged binary, bundle, and scene results. The shell includes menu, toolbar, status bar, workspace presets, and configurable panels. The alternative serialized docking host exists behind `engine_editor/dock-shell`; the default `cargo run -p editor` dependency enables the terminal feature but not docking.

Primary sources: `editor/src/main.rs`, `editor/src/play_process.rs`, `crates/engine_editor/src/`, `crates/editor_code/src/lib.rs`, and `crates/editor_lsp/src/lib.rs`.

## AI and Automation Boundaries

These layers are partial rather than a finished assistant:

- `editor_ai` defines Ask/Code/Scene/Debug/Build modes, provider-neutral chat/session types, bounded deterministic project indexing, provider configuration, and credential-store interfaces. On macOS, credentials are accessed through the OS keychain. No HTTP provider transport is implemented.
- `editor_tools` defines auditable read/search/patch/scene/build/run requests, permission tiers, bounded JSON-RPC envelopes, action logs, project-root path checks, and stale-file/explicit-approval protection for patches.
- The editor can apply selected scene tool edits through its history/dirty-state path, but the visible AI Assistant panel remains a placeholder and there is no complete agent loop or external MCP server.

Primary sources: `crates/editor_ai/src/lib.rs`, `crates/editor_tools/src/lib.rs`, and `crates/engine_editor/src/state.rs`.

## Sample Project
- `games/island`: playable procedural island, asset baker, generated glTF/PNG/WAV files, scene metadata, third-person controls, physics, animation events, pause UI, and positional ambience. The current game still builds its world and character in code; loading the baked project and importing the skinned character are documented gaps in `games/island/src/main.rs`.

## Stubs and Explicit Gaps

- `engine_network` is a stub; there is no client/server transport, synchronization, RPC, or prediction implementation.
- `engine_scripting` is a stub; gameplay is ordinary Rust through the facade and ECS APIs.
- AI provider networking and the finished Studio assistant are absent.
- Cross-compilation, automatic updates, public-distribution signing configuration, and bundled rust-analyzer are outside the local runtime implementation.
- Several source-level module status comments describe older milestones; this reference follows current call paths and public APIs where those comments conflict with working integration.

## Verification Surface

The workspace contains 1,275 `#[test]` annotations across runtime, editor, player, and Island sources. CI runs formatting, Clippy with warnings denied, all-feature workspace tests and doctests, documentation, dependency policy checks, Windows checks, Linux builds/tests, and release packaging. The relevant local commands are documented in `AGENTS.md`.
