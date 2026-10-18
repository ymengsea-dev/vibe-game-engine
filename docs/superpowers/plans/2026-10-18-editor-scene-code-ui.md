# Editor Scene and Code UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the existing editor use focused Scene and Code layouts styled after the supplied prototypes without adding mock-only functionality.

**Architecture:** Keep `Workspace::World` and `Workspace::Code` as the persisted modes, but expose them as Scene and Code in primary chrome. Split the current shell drawing into mode-specific layout helpers that reuse every existing panel renderer and continue reporting the Monaco rectangle to the native host.

**Tech Stack:** Rust 2024, egui, egui-wgpu, winit/WGPU, existing Monaco native webview integration.

**Spec:** `docs/superpowers/specs/2026-10-18-editor-scene-code-ui-design.md`

## Global Constraints

- Reuse existing editor panels, state, session persistence, and Monaco placement; add no dependency.
- Render only controls backed by real behavior; do not add Lua, live-runtime, pause, step, or placeholder controls.
- Preserve project actions, selection, undo history, asset drops, diagnostics, play/stop, and build configuration.
- Keep AI/Debug auxiliary behavior reachable through existing recovery/menu paths, but remove them from primary chrome.
- Run focused editor tests before broader workspace checks.

## Review Focus

- Mode switching must retain the world and undo history; test it through `EditorState::set_workspace`.
- A saved legacy `Workspace::World` session must still restore successfully after Scene becomes its display name; test existing session round-trip behavior.
- The Code Monaco rectangle must be cleared when Scene mode is shown so a native webview cannot cover the viewport; test the state contract where practical and manually verify it.
- Panel collapse/reset must preserve the active mode's panel visibility; test reset/mode layout behavior.
- Unsupported controls must not be presented as available; manually verify pause, Game/Shaded tabs, and prototype Lua/live-runtime controls are absent.

### Task 1: Primary Scene and Code modes

**Files:**
- Modify: `crates/engine_editor/src/chrome.rs: Workspace labels, primary-mode list, Scene/Code visibility presets, and unit tests`
- Modify: `crates/engine_editor/src/state.rs: workspace/session tests only if the revised preset changes their assertions`

**Interfaces:**
- Produces: `Workspace::PRIMARY: [Workspace; 2]` and `Workspace::primary_label(self) -> &'static str`.
- Consumes: existing `Workspace::ALL`, `Workspace::visibility`, `EditorState::set_workspace`, and session serialization.

- [ ] **Step 1: Write failing chrome tests**

Add a test that asserts `Workspace::PRIMARY == [Workspace::World, Workspace::Code]`, with primary labels `"Scene"` and `"Code"`. Add assertions that Scene has hierarchy, inspector, asset browser, and console but not the code panel; Code has asset browser, code, and console but not hierarchy or inspector.

- [ ] **Step 2: Run the focused chrome test to verify it fails**

Run: `cargo test -p engine_editor primary_modes --lib`

Expected: FAIL because `PRIMARY` and `primary_label` do not exist and World still shows the code panel.

- [ ] **Step 3: Implement the mode API and visibility presets**

In `chrome.rs`, add `Workspace::PRIMARY` and `Workspace::primary_label` without changing serialized enum variants or `Workspace::ALL`. Change only World and Code presets to the required Scene/Code layouts. Keep `Workspace::label` for auxiliary Window menu entries and existing session compatibility. Update affected state tests to assert the new World/Code preset shapes while retaining the no-world-mutation invariant.

- [ ] **Step 4: Run focused chrome and state tests**

Run: `cargo test -p engine_editor primary_modes workspace --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/engine_editor/src/chrome.rs crates/engine_editor/src/state.rs
git commit -m "feat(editor): define scene and code modes"
```

### Task 2: Prototype-shaped shell layouts

**Files:**
- Modify: `crates/engine_editor/src/shell.rs: toolbar, scene/code layout helpers, viewport header, panel titles, Monaco rectangle lifecycle, and UI tests`
- Modify: `crates/engine_editor/src/theme.rs: only the existing dark egui style values needed for the compact prototype treatment`
- Test: existing unit-test modules in `shell.rs` and `theme.rs`

**Interfaces:**
- Consumes: `Workspace::PRIMARY`, `Workspace::primary_label`, `PanelVisibility`, `Viewport`, and the existing panel renderer functions.
- Produces: a Scene layout with hierarchy/assets, viewport, inspector, and output; a Code layout with Explorer, Monaco, editor viewport, and output. Both maintain `EditorState::code_panel_rect` and `code_panel_visible` correctly.

- [ ] **Step 1: Write failing pure-layout/style tests**

Add tests for the primary toolbar source of truth and any extracted helper that maps the active workspace to Scene/Code. Add a theme assertion for the compact dark panel treatment only if a visual value changes. Do not test egui draw internals.

- [ ] **Step 2: Run the focused tests to verify they fail**

Run: `cargo test -p engine_editor primary_toolbar --lib`

Expected: FAIL because the toolbar still iterates `Workspace::ALL` and no primary-mode helper/test exists.

- [ ] **Step 3: Implement the Scene shell**

Refactor the non-docking shell into focused helpers rather than duplicating panel behavior. The Scene helper must draw: left Hierarchy above Asset Browser; central viewport with a compact real viewport toolbar (2D/3D, real gizmo mode, transform-space, overlays); right Inspector; and bottom Console/Problems/Output/Terminal. Reuse existing hierarchy, assets, inspector, terrain, viewport, and bottom-tab functions. Remove disabled Game/Shaded tabs and hide Pause. Preserve drag-drop, resize/collapse, inspector add-component, asset preview, and error reporting.

- [ ] **Step 4: Implement the Code shell**

Draw the bottom output area first, then a left project/code Explorer using the existing asset browser/open-file path, a right editor viewport, and a central Monaco panel. Ensure the native Monaco rectangle covers exactly the central panel body; when the panel is unavailable or Scene is active, clear the rectangle and visibility. Keep save forwarding and resize hiding unchanged. Keep AI/Debug only as auxiliary Window-menu behavior, not primary toolbar modes.

- [ ] **Step 5: Apply compact prototype visual treatment**

Adjust existing theme spacing, panel header composition, separators, and button grouping only as necessary to visually match the dark prototypes. Do not import web fonts, copy static HTML controls, or create fake runtime indicators.

- [ ] **Step 6: Run focused tests to verify they pass**

Run: `cargo test -p engine_editor --lib`

Expected: PASS.

- [ ] **Step 7: Manually verify the rendered editor**

Run: `cargo run -p editor`

Expected: Scene and Code buttons switch layouts; Scene supports hierarchy, assets, inspector, viewport gizmos, console, play/stop; Code presents Explorer, Monaco, editor viewport, and output; no disabled prototype-only controls are visible.

- [ ] **Step 8: Commit**

```bash
git add crates/engine_editor/src/shell.rs crates/engine_editor/src/theme.rs
git commit -m "feat(editor): adopt scene and code layouts"
```

### Task 3: Regression verification

**Files:**
- Modify: no production files unless verification exposes a defect.

**Interfaces:**
- Consumes: completed Scene/Code mode and shell changes.
- Produces: verification evidence only.

- [ ] **Step 1: Format the changed editor crates**

Run: `cargo fmt --all -- --check`

Expected: PASS.

- [ ] **Step 2: Run editor and workspace quality checks**

Run: `cargo test -p engine_editor --all-features && cargo clippy -p engine_editor --all-targets --all-features -- -D warnings`

Expected: PASS.

- [ ] **Step 3: Run the workspace test suite when the focused checks pass**

Run: `cargo test --workspace --all-features`

Expected: PASS.

- [ ] **Step 4: Review the final diff**

Confirm only editor UI/style/tests and the plan changed; preserve unrelated staged engine and game work.

- [ ] **Step 5: Commit a validation fix only if verification required one**

Stage only the named editor files that the failing verification required, then
commit them with `fix(editor): complete scene and code UI validation`.

