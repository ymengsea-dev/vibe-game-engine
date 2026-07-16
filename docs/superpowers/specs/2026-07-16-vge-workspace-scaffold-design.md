# VGE Workspace Scaffold — Design

**Date:** 2026-07-16
**Status:** Approved

## Goal

Initialize the Vibe Game Engine (VGE) repository with a professional,
industry-standard Cargo workspace matching the architecture defined in
`docs/ARCHITECTURE.md`, ready for Milestone 1 development.

## Decisions (user-approved)

| Decision | Choice |
|---|---|
| Folder name | `vge` (kebab/short; full title lives in README) |
| Crate layout | Full skeleton — all 15 engine modules as compile-clean stubs |
| Crate naming | `engine_*` prefix (`engine_core`, `engine_renderer`, …) |
| Version control | Git, MIT OR Apache-2.0 dual license |

## Structure

- `Cargo.toml` — workspace root: resolver 3, edition 2024, shared
  `[workspace.package]`, `[workspace.dependencies]`, `[workspace.lints]`.
- `crates/engine_*` — 15 stub crates, each with a doc-only `lib.rs`
  stating purpose and target milestone.
- `crates/engine` — facade crate re-exporting every subsystem; game
  projects depend on this single crate (`use engine::prelude::*`).
- `game/` — example binary exercising the engine, plus asset directories
  (`assets/`, `scenes/`, `prefabs/`, `materials/`, `shaders/`,
  `textures/`, `models/`, `audio/`, `config/`).
- `docs/ARCHITECTURE.md`, `docs/ENGINEERING.md` — copied from Dev
  Documents so the repo is self-contained.

## Key choices

1. **Facade crate** — internal crate boundaries can evolve without
   breaking games (Bevy pattern).
2. **Workspace dependency table** — every external version pinned in one
   place; deps wired into crates only when a milestone needs them, so the
   first build stays fast and versions are re-verified at wiring time.
3. **Lints enforce ENGINEERING.md** — `clippy::unwrap_used`,
   `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro` denied
   workspace-wide; `unsafe_code` denied; `missing_docs` warns. Tests are
   exempt via `clippy.toml` (`allow-unwrap-in-tests`, etc.).
4. **Build profiles** — dev builds optimize dependencies at `-O3` while
   keeping engine crates fast to compile (`profile.dev.package."*"`).
5. **No CI yet** — added when a remote exists.

## Verification

- `cargo build` succeeds across the workspace
- `cargo clippy --all-targets -- -D warnings` clean
- `cargo fmt --check` clean
- Initial git commit contains the full scaffold

## Out of scope

Any engine functionality. Milestone 1 (window + clear color) is the next
unit of work and follows the iteration workflow in `docs/ENGINEERING.md`.
