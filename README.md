# Vibe Game Engine (VGE)

> A lightweight, Rust-first game engine focused on performance, simplicity,
> and modern tooling.

VGE is a modular 2D/3D game engine for Windows, macOS, and Linux. Gameplay
is written entirely in Rust — no proprietary scripting language. The editor
exists to improve productivity, not to replace code.

## Status

**Pre-milestone scaffold.** The workspace structure is in place; engine
implementation begins with Milestone 1 (window + GPU clear color).

## Architecture

```
Game Project
     │
┌────┴─────────────┐
│                  │
Gameplay (Rust)   Editor (egui)
│                  │
└────┬─────────────┘
     │
Engine Runtime
(core · ecs · renderer · physics · audio · asset · scene · animation · ui · platform)
```

| Concern | Technology |
|---|---|
| Windowing / input | [winit](https://crates.io/crates/winit) |
| Rendering | [wgpu](https://crates.io/crates/wgpu) + WGSL |
| ECS | [bevy_ecs](https://crates.io/crates/bevy_ecs) |
| Math | [glam](https://crates.io/crates/glam) |
| Physics | [rapier](https://rapier.rs) (2D + 3D) |
| Audio | [kira](https://crates.io/crates/kira) |
| Editor UI | [egui](https://crates.io/crates/egui) |
| Assets | glTF 2.0, serde + RON |

## Workspace Layout

```
vge/
├── crates/
│   ├── engine/            # Facade — games depend on this single crate
│   ├── engine_core/       # Startup, main loop, timing, config
│   ├── engine_platform/   # Window, input, OS integration
│   ├── engine_renderer/   # wgpu rendering
│   ├── engine_ecs/        # Entity Component System
│   ├── engine_scene/      # Scenes, prefabs, hierarchy
│   ├── engine_asset/      # Import pipeline, hot reload
│   ├── engine_physics/    # rapier integration
│   ├── engine_animation/  # Skeletons, blend trees, state machines
│   ├── engine_audio/      # Music, SFX, spatial audio
│   ├── engine_ui/         # Runtime game UI
│   ├── engine_editor/     # egui editor tools
│   ├── engine_scripting/  # Gameplay-facing API surface
│   ├── engine_network/    # Multiplayer (future)
│   ├── engine_ai/         # MCP server, AI integration
│   └── engine_utils/      # Shared helpers
└── game/                  # Example game exercising the engine
```

## Building

Requires stable Rust (managed by `rust-toolchain.toml`).

```sh
cargo build            # build everything
cargo run -p game      # run the example game
cargo test             # run all tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Engineering Standards

No `unwrap`/`expect`/`panic!` in production code (enforced by workspace
lints), full error handling with `Result` + `thiserror`, tests for every
feature, and clippy/rustfmt clean builds. `docs/` is reserved for generated
API documentation.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
