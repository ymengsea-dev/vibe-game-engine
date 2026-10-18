# Prototype capabilities not implemented in the editor

These controls from the supplied Scene and Code UI prototypes must stay hidden
until the underlying behavior exists. They are not placeholders for this UI
replacement.

## Scripting

- **Lua Script inspector and script picker:** hidden because there is no Lua
  component, runtime, or script asset model. Restore after defining script
  ownership, lifecycle, serialization, and error reporting.
- **Open in Script Editor for Lua:** hidden because Monaco can edit project
  text, but no Lua attachment exists. Restore after Lua assets and attachment
  route their paths to Monaco.
- **Lua language/runtime:** hidden because the current game/editor code is
  Rust and no embedded Lua VM exists. Restore after choosing and integrating a
  VM, sandboxing, API, build/package flow, and diagnostics.
- **Script hot reload:** hidden because there is no player-to-editor script
  reload protocol. Restore after a safe runtime transport and reload lifecycle
  exist.

## Live runtime controls

- **Live game viewport in Code mode:** hidden because the available viewport
  renders the editor world, not an executing player. Restore after designing
  player embedding or frame streaming, including input, lifecycle, and errors.
- **Runtime entity status/details:** hidden because the editor has no runtime
  inspection channel. Restore after adding a versioned player/editor inspection
  protocol.
- **Pause and step:** hidden because Play launches a separate process with no
  pause protocol. Restore after the player supports pause/step commands and
  state acknowledgement.

## Asset presentation

- **Image metadata cards per thumbnail:** hidden because the browser has an
  asset index and preview but no card model for every entry. Restore after the
  index supplies defined metadata for each asset kind.

The Inspector **Add Component** menu is real for registered components and may
be shown. The top-level Component menu is not a substitute for it and stays
hidden until it uses the same registry.

