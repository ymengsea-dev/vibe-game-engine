# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Layout

This root directory is a container, not a repository:

- `vge/` — the actual project. Git repo lives here. The product is **RustyEngine Studio**: a lightweight, editor-first, AI-assisted Rust 2D/3D game studio (one window, dockable panels, integrated Rust code editor, IDE-style AI agent) built on top of a Rust engine runtime. The `vge/` dir name and `engine*` crate names are the current implementation; the product/crate rename is tracked, not urgent.
- `Dev Documents/` — **private** design docs, deliberately outside the repo so they never reach GitHub. Three live docs:
  - `Product Feature Requirement.md` — the product: what RustyEngine Studio is, every feature area + release tier (MVP/V1/V2), the AI tool API, the tech stack, the architecture, the acceptance gate. Source of truth for scope.
  - `Tasks Workflow.md` — the task tracker: one checkbox per `/build-feat-vge` iteration, "Current Position" marks progress, plus the how-we-work notes. Update it when a feature completes.
  - `Engine Completion Spec.md` — the engineering spec: code-verified gap analysis (every claim cited `file:line`) plus 19 tasks `T-01`–`T-19` in four phases, each with acceptance criteria and required tests. Source of truth for *what is actually built vs. only claimed built*. Read before proposing engine work.
  - `prototype.png` — canonical UI reference (the studio layout every panel/menu decision follows).
  - `trash/` — superseded docs (original vendor spec content, old engine audit, old how-to). Archive only; do not cite.

New specs/design docs go in `Dev Documents/`, never inside `vge/`. `vge/docs/` is reserved for generated API documentation and stays empty unless the user asks.

## Hard Rules

- **No git operations (add/commit/amend/push/anything) unless the user explicitly instructs it.** Report changes and wait.
- **One task per iteration** — one numbered task from `Dev Documents/Engine Completion Spec.md` (`T-`/`S-`/`R-`) or one tracker checkbox, built end to end with all its sub-features. Never a whole milestone or release tier at once, and never half a task. (Rule changed 2026-09-04 from one-sub-feature-per-iteration, which was too slow.) Follow the 9-step workflow in the `/build-feat-vge <feature>` skill (`.claude/skills/build-feat-vge/`) (understand → design → implement → verify → perf review → memory review → reliability review → docs → summary) — use it for any engine feature implementation.
- Known issue: initial commit `636d183` contains doc files in git history; must be amended before any GitHub push.

## Commands

All run from `vge/` (toolchain pinned by `rust-toolchain.toml`; cargo is in `~/.cargo/bin`):

```sh
cargo build                              # build workspace
cargo test                               # all tests
cargo test -p engine_core               # tests for one crate
cargo test -p engine_core test_name     # single test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

All five verification steps (build, test, clippy, fmt, run) must pass before a feature is considered done.

## Architecture

Cargo workspace (edition 2024): `crates/engine_*` + `crates/editor_code` (19 crates incl. `engine` facade, `engine_editor`, `engine_editor_derive`, `engine_project`) + `editor/` shell binary.

- **`engine`** — facade crate. Game projects and the editor depend on this single crate (`use engine::prelude::*`); it re-exports every subsystem so internal crate boundaries can evolve freely. When a subsystem gains public API, re-export the common types through `engine::prelude`.
- **`engine_core`** — startup, main loop, timing, config. Everything eventually depends on core.
- **`engine_*` subsystem crates** — platform, renderer, ecs, scene, asset, physics, animation, audio, ui, editor, scripting, network, ai, utils. The `engine_*` layer is built **as libraries** (former roadmap Stages 1–5): each subsystem exists and is tested in isolation. It is **not integrated** — see "Current state" below and `Dev Documents/Engine Completion Spec.md`.

**Studio-layer target** (spec §3.1 — not yet built): split the editor into `editor_core` / `editor_ui` / `editor_scene` / `editor_code` (embedded editor + rust-analyzer LSP bridge) / `editor_ai` (provider abstraction + agent loop); add `editor_tools` (safe tool API shared by AI and automation), `project` (project format, settings, build targets), and `apps/{studio, player}`.

Product rules from the spec:

- **The editor is a client of the engine.** It consumes public engine services the same way a game runtime does — no editor-specific state leaks into shipped games. Runtime builds must not depend on editor or AI crates (NFR-004).
- **All scene mutations go through undoable commands.** The AI agent calls the same command / `editor_tools` API as human editor actions — shared validation, history, and undo (spec §3.2, §10).
- **glTF 2.0 is the engine-native 3D format.** Do not hand-write an FBX importer; if FBX is ever needed, shell out to a glTF-conversion step at import time. (The mockup's `.fbx` labels are illustrative.)
- **AI permissions:** destructive/high-impact actions (delete, file overwrite, shell, network) are gated; tools are project-root scoped; provider API keys live in OS secret storage, never in model context (spec §10.2, NFR-007/008).

Key wiring conventions:

- **External dependencies** (winit, wgpu, bevy_ecs, glam, rapier, kira, egui, …) are declared once in the root `Cargo.toml` `[workspace.dependencies]` table but wired into a crate only when a milestone needs them. Verify the pinned version is current when first wiring one in.
- **Lints enforce the engineering rules at compile time** (root `Cargo.toml` `[workspace.lints]`): `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`, and `unsafe_code` are denied; `missing_docs` warns. Tests are exempt via `clippy.toml`. Production code returns `Result` with `thiserror` errors — never panics.
- **Build profiles**: dev builds compile engine crates at `-O1` but dependencies at `-O3` (`profile.dev.package."*"`), keeping iteration fast while wgpu/physics stay usable.

Current state: engine layer built **as libraries, not as an assembled engine** (audited 2026-09-04; 976 tests pass). Every `engine_*` subsystem works in isolation, but the integration between them does not exist: `run_game` wires only render + input + particles + vegetation (no physics step, audio, animation, sprites, or UI), the scene round-trip drops `MeshRenderer`/`Sprite` so a saved scene cannot be displayed, the editor Scene view draws every entity as an identical grey cube, and `apps/player` renders nothing. Fixing this is phase P0 of `Dev Documents/Engine Completion Spec.md` — read it before proposing engine work.

Studio MVP in progress — done: docked shell (menu/tool/status bars, workspace presets), project system (`engine_project` + `.studio/session.ron`), unsaved-change tracking (`DirtyState` + close guard), integrated code editor spike (`editor_code` = Monaco in a `wry` webview floated over the Code panel). Next: Console/Problems/Output panel. See `Dev Documents/Tasks Workflow.md` "Current Position".
