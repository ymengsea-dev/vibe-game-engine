# Prototype capabilities not implemented in the editor

These controls from the supplied Scene and Code UI prototypes must stay hidden
until the underlying behavior exists. They are not placeholders for this UI
replacement.

| Prototype capability | Why it is hidden | Needed before restoration |
| --- | --- | --- |
| Lua Script inspector section and script picker | No Lua component, runtime, or script asset model exists. | Define script asset/component ownership, runtime lifecycle, serialization, and errors. |
| “Open in Script Editor” for Lua | Monaco can edit project text, but no Lua attachment exists. | Implement Lua assets/attachment, then route its path to Monaco. |
| Lua language/runtime | Current game/editor code is Rust; no embedded Lua VM exists. | Choose and integrate a VM, sandboxing, API, build/package flow, and diagnostics. |
| Script hot reload | There is no player-to-editor script reload protocol. | Add a safe runtime transport and reload lifecycle. |
| Live game viewport in Code mode | The available viewport renders the editor world, not an executing player. | Design player embedding or frame streaming plus input, lifecycle, and errors. |
| Runtime entity status/details | The editor has no runtime inspection channel. | Add a versioned player/editor inspection protocol. |
| Pause and step buttons | Play launches a separate process with no pause protocol. | Add player pause/step commands and state acknowledgement. |
| Image metadata cards per thumbnail | The browser has an asset index and preview but no card model for every entry. | Extend indexed metadata and define fields for each asset kind. |

The Inspector **Add Component** menu is real for registered components and may
be shown. The top-level Component menu is not a substitute for it and stays
hidden until it uses the same registry.

