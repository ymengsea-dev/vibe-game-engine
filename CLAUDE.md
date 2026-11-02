# CLAUDE.md

Guidance for Claude Code in this repository. `AGENTS.md` holds the shared engineering rules (written for Codex, which worked on this repo before); follow it too — this file adds Claude-specific context and overrides it where they differ.

## What this is

**RustyEngine Studio** — an editor-first 2D/3D game studio (egui shell, dockable panels, Monaco code editor, separate player process) on a Rust engine runtime. The repo root `vge/` *is* the git repo. Studio and engine are Rust; **game projects choose Lua (default for new projects) or Rust for gameplay** at creation time — one language per project, no mixing or conversion.

The feature list is `docs/dev/01_STUDIO_FEATURES.md` ("RustyEngine Studio Features"). It is the **single** feature document: one section per area, one table row per feature, saying in plain language what the feature is, what it's used for, and its status (✅/🟡/⬜).

- Keep it short and beginner-readable. No code paths, limits or implementation detail in it.
- Don't create other feature docs. The user removed a multi-page `docs/features/` set as too complex.
- When a feature lands or changes status, update its row in the same change.

Docs are split by audience (index: `docs/README.md`): `docs/user/` is product documentation for game makers, and `docs/dev/` is for people building Studio (its `00_README.md` holds the doc-maintenance rules). User docs:

- `docs/user/01_KEYBOARD_SHORTCUTS.md`: every shortcut and mouse action, by panel, plus a "Not available yet" list.
- `docs/user/02_SCENE_VIEW_GUIDE.md`: every Scene View option (gizmo tools, 3D/2D, overlays, Terrain, Tilemap, Environment) and what choosing it does.
- `docs/user/03_STORAGE.md`: Settings → Storage: what Studio may clean (build caches, temp files, old engine copies), what it never touches, and automatic cleanup.

Whenever a change adds or changes a shortcut, mouse action, menu option, overlay or on-screen marker, update the matching doc in the same change. Tell the user about new shortcuts and point to the doc. Verify each documented behaviour against the code; document limitations honestly.

Implementation plans and design specs are managed by the superpowers skill under `.superpowers/{specs,plans}/` (gitignored, at the repo root). The detailed Lua/Rust design, including the old roadmap text, quick starts and limits, is in `.superpowers/specs/2026-10-25-lua-rust-game-creation-design.md`. `docs/dev/02_ENGINE_FEATURES.md` is the older engine-runtime inventory (2026-10-13).

## Hard rules

- **No git operations (add/commit/stage/amend/push) unless the user explicitly asks.** Report changes and wait. When asked to stage, the user often wants editor changes and game (`games/island`) changes staged/committed separately.
- Known issue: initial commit `636d183` contains private doc files in history; must be rewritten before any public push.
- Don't overwrite user files silently. Existing patterns to follow: `create_new` backups (`project.ron.vN.bak`, `Cargo.toml.sdk-<ver>.bak`, `studio_api.lua.legacy.bak`), `engine::scene::write_atomic`, refuse-if-differs instead of clobber.
- Production code: no `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`dbg!`/`unsafe` (denied by `[workspace.lints]`; tests exempt via `clippy.toml`). Return `thiserror` `Result`s.
- Runtime builds (`apps/player`, game projects) must not link editor or AI crates: they use `engine` with `default-features = false`. The `studio` feature pulls in `engine_editor`; `lua` enables `engine_scripting/lua` (mlua 0.12, vendored Lua 5.4).
- glTF 2.0 is the native 3D format; no hand-written FBX importer.
- **Building or changing any UI: follow `docs/dev/04_UI_STYLE.md`.** It covers the Tokyo Night palette (`theme::NIGHT`, no hard-coded colours), the soft Apple-like shapes (fills, not outlines), SF Pro and Menlo type with semibold titles only, the shared controls in `engine_editor/src/widgets.rs`, and the writing rules. Update it when a token, control or rule changes.
- Look at UI changes before handing them over: `crates/engine_editor/src/ui_snapshot.rs` renders egui UI to a PNG on the headless GPU. Run `VGE_SNAPSHOT_DIR=<dir> cargo test -p engine_editor -- --ignored snapshot`, then view the PNGs (`settings.png`, `settings-automatic.png`, `scene-chrome.png`, `inspector.png`, `inspector-narrow.png`, `inspector-empty.png`, `scrub-cursor.png`, `console.png`, `console-narrow.png`, `panel-grips.png`). Add a snapshot test for any new window or panel.
- The user verifies UI by running the editor and sending screenshots. "Done" means visible in `cargo run -p editor`, not only green tests — say explicitly when something was not checked in a running editor/player.

## Commands (run from `vge/`; cargo in `~/.cargo/bin`)

```sh
cargo build --workspace --all-features
cargo run -p editor [-- <project-dir>]    # Studio; no arg → restored session or ./RustyEngineProject (gitignored scratch)
cargo run -p player -- --project <dir> [--scene scenes/x.ron]   # or: player <export-dir>
cargo test --workspace --all-features     # slow: several minutes incl. doctests
cargo test -p engine_scripting --features lua <name>
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo deny check                          # licenses/advisories (deny.toml)
cargo build -p editor -p player --release && cargo run -p studio_packager -- target/release target/<new-dir>
```

zsh note: don't `echo ======` (zsh `=` expansion errors); use `echo '---'`.

Disk note: Cargo never prunes `target/`. On 2026-11-01 it reached 304 GB (incremental snapshots plus every test binary) and filled the disk. The dev profile now keeps line tables only (`debug = "line-tables-only"`, no debug info for dependencies). Check `du -sh target` after heavy sessions, and run `cargo clean` (it removes only `vge/target`) when it grows past a few tens of GB.

## Layout

Workspace members: `crates/*`, `apps/*`, `games/*`, `editor`.

- `crates/engine` — facade; games use `engine::prelude::*`. Re-export new public API here.
- `crates/engine_*` — subsystems (core, platform, renderer, ecs, scene, asset, physics, animation, audio, ui, scripting, project, network, ai, utils) plus `engine_editor` (+ `_derive`), `editor_code` (Monaco in a `wry` webview), `editor_lsp` (rust-analyzer / LuaLS bridge), `editor_ai`, `editor_tools`.
- `crates/engine_scene` — scene format (v5: stable object IDs, script paths), validation, `rules.rs` (visual event/action rules + dispatcher shared by Lua and Rust), `ui.rs` (authored game UI), `sprite_atlas.rs`.
- `crates/engine_scripting/src/lua.rs` — the sandboxed Lua VM:
  - Libraries: `StdLib` table/string/utf8/math only; `load`/`require`/`dofile` removed.
  - Limits: 64 MiB memory, 1M instructions per callback (`pcall`/`xpcall` wrapped so they can't swallow it), 1,024 commands per dispatch.
  - Scripts return a callback table; engine calls go through queued `ScriptCommand`s, never raw ECS access.
  - Adding to the `engine.*` API also means changing the Lua annotations (`STUDIO_API_LUA` in `engine_project`). Never edit that text in place: freeze the old text, bump `STUDIO_API_VERSION`, and extend `generated_annotation_backup`.
- `crates/engine_project` — `project.ron` manifest (v3: `project_id`, `gameplay_language`), migrations, starters (`starter.rs`), Lua/Rust export (`build.rs`).
- `editor/` — Studio binary: winit/egui host, macOS app/menu (`mac_app.rs`, `native_menu.rs`), Play process management (`play_process.rs`), embedded Rust SDK snapshot (`build.rs` → `sdk_files.rs`) and SDK upgrade.
- `crates/engine/src/gameplay/` — `GameplayRuntime`, the language-neutral game runtime shared by Lua and Rust games: applies `GameCommand`s (from `engine_scripting::command`), dispatches visual rules, samples collisions, switches scenes. New gameplay features go here, not in the player.
- `apps/player` — standalone player: thin Lua glue (script loading, input/scene snapshots, callbacks, diagnostics, saves) over `GameplayRuntime`; also runs exports.
- `apps/studio_packager` — stages a portable host-OS distribution (Studio + player + Monaco + LuaLS 3.19.2, SHA-256 pinned).
- `games/island` — Rust showcase/regression project. Opening it in Studio migrates its manifest; keep it working.
- `.superpowers/` — design specs and implementation plans (gitignored, repo root). Five files that used to live in `docs/superpowers/` are still tracked in git history until their removal is committed.

## Where the previous work left off (as of 2026-10-31)

Codex implemented the roadmap plan ("Beginner-friendly Studio with Rust or Lua game projects") under a long-running goal that stopped on usage limits, not completion. Codex's own estimate: ~60%. All of it is **uncommitted** in the working tree (~80 files, +12k lines).

Claude has since done the following, also uncommitted:

- fixed the `pcall` instruction-limit bypass;
- replaced the roadmap with the one-file feature summary `docs/dev/01_STUDIO_FEATURES.md`;
- added numeric variables and single conditions to visual rules, plus Lua `get_var`/`set_var`;
- versioned the Lua annotation migration (v2).

Not done: combined conditions for rules, Blueprint-style graph beyond the event/action canvas, visual shader graph, terrain texture painting, tile layers/auto-tiling, skeletal animation authoring, Windows/Linux packaging and clean-machine verification, and running-editor checks of the 2D/3D starters. Lua and Rust games now share `engine::gameplay::GameplayRuntime` (2026-10-31); the Rust starter template (`editor/assets/rust_starter.rs`) is compile-checked as the `editor` example `rust_starter`.

Codex history for this repo, if context is needed: `~/.codex/sessions/2026/10/22/rollout-2026-10-22T20-31-13-01a163ce-*.jsonl` (large JSONL; filter with `jq` on `.payload.role=="user"`).
