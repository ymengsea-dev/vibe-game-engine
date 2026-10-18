# Existing editor capabilities absent from the supplied prototypes

These features already exist but are not represented in the proposed Scene or
Code layouts. The UI replacement must retain them in menus or auxiliary panels,
or deliberately keep them out of the two primary modes; it must not silently
remove their behavior.

| Existing capability | Current location/behavior | UI treatment in this change |
| --- | --- | --- |
| AI Assistant workspace/panel | `Workspace::Ai`; the panel is currently a placeholder. | Hide from primary mode switch; retain no new affordance. |
| Debug workspace and CPU profiler | `Workspace::Debug` shows the profiler strip. | Hide from primary mode switch; retain Window/layout recovery. |
| Asset preview strip | Asset Browser has a resizable selected-asset preview. | Keep in Scene Assets panel. |
| Terrain and vegetation authoring | Collapsible tools in the Scene view. | Keep in Scene mode, visually compact. |
| View overlays | Colliders, lights, cameras, ground grid, audio emitters, nav grid. | Keep under Scene View/viewport controls. |
| Build configuration and Build & Export | Toolbar selector and project actions. | Keep in shared chrome/menu. |
| Search output tab | Bottom panel tab alongside Console/Problems/Output/Terminal. | Keep in bottom panel. |
| Terminal feature gate | Terminal tab shows a notice without the feature. | Keep it; do not promise a terminal where unavailable. |
| Lighting inspector tab | Inspector secondary tab is currently a placeholder. | Do not add a prototype control; preserve current behavior. |
| Asset import, folder creation, context actions | Asset menu/browser actions. | Keep in Scene Assets panel and menus. |
| Undo/redo, multi-selection, prefab actions | Existing menu/hierarchy/history behavior. | Retain through existing menus and hierarchy. |
| Panel collapse, layout reset, session persistence | Current shell/session behavior. | Preserve as recovery and personalization behavior. |

