# Existing editor capabilities absent from the supplied prototypes

These features already exist but are not represented in the proposed Scene or
Code layouts. The UI replacement must retain them in menus or auxiliary panels,
or deliberately keep them out of the two primary modes; it must not silently
remove their behavior.

## Auxiliary workspaces

- **AI Assistant:** the existing `Workspace::Ai` panel is a placeholder. Hide
  it from the primary mode switch; add no replacement control.
- **Debug workspace and CPU profiler:** `Workspace::Debug` exposes the
  profiler strip. Hide it from the primary switch but preserve Window/layout
  recovery.
- **Lighting inspector tab:** this secondary Inspector tab is a placeholder.
  Preserve its current behavior without adding a prototype control.

## Scene authoring and assets

- **Asset preview strip:** retain the Asset Browser's resizable selected-asset
  preview in Scene mode.
- **Terrain and vegetation authoring:** retain the existing collapsible Scene
  tools in a compact treatment.
- **View overlays:** retain colliders, lights, cameras, ground grid, audio
  emitters, and nav grid under Scene View or viewport controls.
- **Asset import, folder creation, and context actions:** retain them in the
  Scene Assets panel and menus.
- **Undo/redo, multi-selection, and prefab actions:** retain them through the
  existing menus and hierarchy.

## Project and layout tools

- **Build configuration and Build & Export:** retain the current toolbar
  selector and project actions in shared chrome or its menus.
- **Search output tab:** retain it beside Console, Problems, Output, and
  Terminal.
- **Terminal feature gate:** retain the tab and its unavailable-feature notice;
  do not promise a terminal where it is not compiled.
- **Panel collapse, layout reset, and session persistence:** preserve these as
  recovery and personalization behavior.

