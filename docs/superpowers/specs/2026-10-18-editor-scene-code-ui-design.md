# Editor Scene and Code UI Design

## Intent and acceptance criteria

Replace the current multi-workspace editor shell with two focused layouts based
on the supplied Scene and Code prototypes. The editor must preserve working
authoring behavior while presenting only controls with an implementation behind
them. A user can switch between Scene and Code without losing selection, open
files, panel sizing, diagnostics, or play state.

Success means Scene mode exposes scene authoring, assets, inspector, viewport,
and output; Code mode exposes project navigation, Monaco, output, and the
existing rendered editor viewport. Existing behavior stays reachable through an
appropriate mode or menu. Unsupported prototype controls are absent, not
disabled or simulated. Two tracked inventories describe omissions in each
direction.

## Scope and non-goals

This is a shell/layout replacement in `crates/engine_editor` plus native Monaco
placement in `editor`. It retains the egui shell, existing viewport, Monaco
integration, session persistence, commands, and project actions. It does not
add a scripting language, runtime transport, game embedding, debugger, new
asset format, or generic docking framework.

The visual target is the prototypes' hierarchy, spacing, panel headers, dark
theme, and named modes. Exact web typography and HTML-only effects are not
requirements; native egui controls remain native.

## Layout and interaction model

### Shared chrome

The top chrome retains existing project/menu actions and shows Run and Stop. It
uses two primary mode buttons, **Scene** and **Code**, mapped to the existing
`Workspace::World` and `Workspace::Code` visibility presets. Workspace changes
only alter panel visibility; they never mutate the scene or undo history.

Run invokes the existing play lifecycle and Stop keeps its current transition.
Pause is hidden because the launched player has no pause protocol. The current
2D/3D selector, gizmo mode, transform-space selector, and build configuration
remain available in Scene mode's viewport toolbar or an existing menu.

### Scene mode

```
Hierarchy | viewport and scene tools | Inspector
Assets    | Console / Problems / Output / Terminal | Inspector
```

Hierarchy keeps selection, creation, rename, deletion, filtering, and prefab
actions. The viewport toolbar presents supported Select/Move/Rotate/Scale,
2D/3D, transform-space, and existing overlays. It continues to accept asset
drops and uses the current gizmo path. Inspector keeps its implemented
component fields and registered-component menu.

The Asset Browser keeps its project tree/grid/list, import, create-folder,
selection, preview, context menu, and code-open behavior. It may use the
prototype's compact treatment but does not promise per-asset cards or image
properties the current asset index cannot supply. The output area retains its
existing tabs and terminal feature gate.

### Code mode

```
Workspace Explorer | Monaco editor | editor viewport
Console / Problems / Output / Terminal
```

The Explorer is the existing asset/project browser constrained to code/project
navigation. Selecting or opening a supported text file forwards its
project-relative path to Monaco exactly as it does today. Monaco retains tabs,
save handling, LSP diagnostics, completion, navigation, rename, and native
webview placement.

The right panel is the existing rendered editor viewport, not a running-game
preview. It remains selection-aware and renders the editor world. No runtime
entity health/status, script hot reload, or player inspection is shown.

### Persistence, errors, and accessibility

The active mode and panel visibility continue to use the session model. The
replacement preserves minimum panel sizes and current collapse/reset behavior.
Native controls retain keyboard focus behavior and tooltips; Monaco keeps focus
and save forwarding. File resolution, import, build, and play failures continue
to report through the existing console/output pathways.

## Unsupported prototype controls

Do not render these until implemented:

- Lua Script inspector section, script picker, and “Open in Script Editor”.
- Lua language/runtime, script attachment, and code hot reload.
- Live game preview and runtime-entity inspector/status display.
- Pause or step playback controls.
- Inactive mock-only component/addition UI that cannot edit a real registered
  component.

The complete request-side list is in
[`../../ui-feature-gaps.md`](../../ui-feature-gaps.md). Existing features that
the prototypes omit are in
[`../../ui-existing-features-not-in-prototype.md`](../../ui-existing-features-not-in-prototype.md).

## Implementation boundaries

`chrome.rs` owns the two public mode choices and visibility mappings.
`shell.rs` owns egui placement, style, and whether a supported control is
visible. `state.rs` remains the source of mode, panel, selection, and open-file
state. `editor/src/main.rs` remains the only owner of floating Monaco and
viewport lifecycle; the shell reports rectangles/state instead of embedding
webview concerns. Existing panel modules continue to own their behavior.

The old AI and Debug workspace buttons leave primary chrome rather than being
recreated as prototype controls. Their current behavior is recorded in the
existing-feature inventory; this avoids deleting their code or claiming
prototype parity. The Window menu/layout reset remains the recovery path for
auxiliary panels.

## Verification

Unit-test Scene/Code mode-to-visibility mapping and any extracted layout
helper. Extend shell tests where they validate control state. Manually verify
mode switching preserves state; scene authoring works; code files open in
Monaco; diagnostics appear; Code mode renders the viewport; play/stop work; and
no Lua/live-preview/pause mock controls are visible. Run formatting, focused
editor tests, then workspace tests and Clippy as practical for changed crates.

## Explicitly deferred work

The missing features in the request-side inventory require separate designs
because they cross runtime/editor boundaries. Exact pixel matching, new
terminal support, and a replacement asset metadata model are outside this
UI-shell change.

