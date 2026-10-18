# VGE — RustyEngine Studio

VGE is a Rust-first 2D/3D game-engine workspace for Windows, macOS, and Linux. It combines a reusable runtime, a native Studio editor, project/export tooling, and a small standalone player. Games are ordinary Rust programs that depend on the public `engine` facade; the editor is optional and is not linked into shipped runtimes.

## What is included

- A `wgpu` renderer with PBR materials, shadows, HDR post-processing, sprites, UI, terrain, particles, instancing, and GPU skinning.
- A `winit` platform layer with keyboard, mouse, gamepad, action-map, and window lifecycle support.
- ECS, physics, audio, scenes/prefabs, asset import and hot reload, saving, navigation, animation, and runtime UI subsystems.
- RustyEngine Studio: scene authoring, asset browsing/import, project settings, code editing, an integrated terminal, play/export workflows, and diagnostics.
- An exported-game player and the Island vertical-slice sample.

For the code-verified capability list and known limits, see [docs/FEATURES.md](docs/FEATURES.md).

## Requirements

- The Rust toolchain specified by `rust-toolchain.toml` (install with [rustup](https://rustup.rs)).
- A graphics driver supported by `wgpu`: Metal on macOS, DirectX 12 on Windows, or Vulkan on Linux.

## Build and verify

Run these commands from the repository root:

```sh
cargo build --workspace --all-features
cargo test --workspace --all-features
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

For a quicker check while working in one crate, use `cargo test -p engine_platform` or replace the package name with the crate you changed.

## Run locally

```sh
cargo run -p editor                         # Launch RustyEngine Studio
cargo run -p island                         # Run the Island sample project
cargo run -p player -- path/to/export-dir   # Run an exported game folder
```

The player expects `main.ron` plus either `assets.pak` or a loose `assets/` directory. The Island project is a sample and acceptance target, not required to use the engine.

## Create, run, and export a project

1. Start Studio with `cargo run -p editor`.
2. Choose **File > New Project…**, enter a project name, then choose its parent location. Studio creates `<location>/<project-name>` with `src/`, `assets/`, `scenes/`, and `project.ron`.
3. Build a scene, add assets, then use the toolbar Play control to run the selected configuration.
4. Choose **File > Build & Export…** and select an empty export directory to stage a distributable game folder.

## Island sample

Run the included third-person sample with:

```sh
cargo run -p island
```

Use WASD to move, hold the left mouse button and drag to orbit the camera, and press Escape for the pause menu. After changing Island's procedural generators, regenerate its checked-in assets with:

```sh
cargo run -p island --bin bake
```

See the [Island developer guide](games/island/README.md) for the showcase
route, project layout, Studio workflow, and focused verification commands.

## Workspace layout

```text
crates/        Reusable engine, editor, project, and tooling crates
editor/        RustyEngine Studio desktop application
apps/player/   Minimal runtime for exported projects
games/island/  Playable sample and asset-baking tool
docs/          Feature reference and project documentation
```

`crates/engine` is the application-facing facade. Runtime projects normally import `engine::prelude::*`; editor-specific functionality stays behind the `studio` feature.

## Development notes

- Format Rust with `cargo fmt`; keep tests beside the code they cover.
- Production code avoids `unsafe`, `unwrap`, `expect`, and `panic!`; return typed errors instead.
- Shared dependency versions belong in the workspace manifest. See [AGENTS.md](AGENTS.md) for contributor guidance.

## License

Dual-licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
