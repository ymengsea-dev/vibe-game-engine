# Rust Game Engine

> A lightweight, Rust-first game engine focused on performance,
> simplicity, and modern tooling.

------------------------------------------------------------------------

# Vision

This project aims to build a personal game engine similar in philosophy
to engines like RAGE, while remaining lightweight and approachable. The
engine is designed for developing both 2D and 3D games using Rust as the
only programming language.

Unlike engines that require proprietary scripting languages or complex
editor workflows, every gameplay system is written directly in Rust. The
editor exists to improve productivity, not to replace code.

The long-term goal is to create a modular engine where every subsystem
is independent, allowing new features to be added without introducing
unnecessary complexity.

Core principles:

- Rust is the only programming language for gameplay.
- Lightweight architecture with minimal dependencies.
- High performance through modern rendering and ECS architecture.
- Cross-platform support for Windows, macOS, and Linux.
- AI-friendly architecture through Model Context Protocol (MCP).
- Modular engine where each subsystem can evolve independently.

------------------------------------------------------------------------

# High Level Architecture

                    Game Project
                         │
            ┌────────────┴────────────┐
            │                         │
       Gameplay (Rust)          Editor (GUI)
            │                         │
            └────────────┬────────────┘
                         │
                   Engine Runtime
                         │
     ┌─────────────────────────────────────────┐
     │ Core                                   │
     │ ECS                                    │
     │ Renderer                               │
     │ Physics                                │
     │ Audio                                  │
     │ Asset System                           │
     │ Scene System                           │
     │ Animation                              │
     │ UI                                     │
     │ Platform Layer                         │
     └─────────────────────────────────────────┘

The engine is divided into independent modules. Every subsystem
communicates through well-defined interfaces instead of direct
dependencies.

This makes maintenance easier and keeps compile times manageable.

------------------------------------------------------------------------

# Technology Stack

## Window and Platform Layer

### winit

The platform layer is responsible for creating application windows,
processing keyboard and mouse input, handling monitors, and
communicating with the operating system.

`winit` is the standard Rust library for this purpose and provides
reliable support for Windows, macOS, Linux, Wayland, and X11.

Responsibilities:

- Window creation
- Keyboard input
- Mouse input
- Gamepad events (future)
- DPI awareness
- Event loop
- Monitor detection

------------------------------------------------------------------------

## Graphics API

### wgpu

The renderer is built on top of `wgpu`.

Instead of targeting a single graphics API, `wgpu` automatically
supports multiple modern graphics backends.

Supported APIs:

- Vulkan
- Metal
- DirectX 12
- OpenGL (fallback)
- WebGPU

This allows the engine to remain portable without maintaining multiple
rendering implementations.

Responsibilities:

- GPU resource management
- Rendering pipeline
- Shader compilation
- Command buffers
- Texture management
- Mesh rendering

------------------------------------------------------------------------

## Shader Language

### WGSL

WGSL is the native shader language for WebGPU and `wgpu`.

Using a single shader language avoids maintaining GLSL, HLSL, and Metal
shader variants simultaneously.

Responsibilities:

- Vertex shaders
- Fragment shaders
- Compute shaders
- Material rendering
- Lighting calculations

------------------------------------------------------------------------

## Entity Component System (ECS)

### bevy_ecs

The ECS manages every object inside the game world.

Instead of large inheritance hierarchies, entities are composed from
reusable components.

Example:

    Player
    ├── Transform
    ├── Mesh
    ├── Camera
    ├── Health
    ├── Inventory
    └── CharacterController

Responsibilities:

- Entity creation
- Component storage
- System scheduling
- Parallel execution
- Events
- Resources

------------------------------------------------------------------------

## Mathematics

### glam

Every game engine depends heavily on vector and matrix mathematics.

`glam` provides SIMD-accelerated implementations for:

- Vector2
- Vector3
- Vector4
- Quaternion
- Matrix4

These are used throughout rendering, animation, physics, and gameplay.

------------------------------------------------------------------------

## Physics

### rapier2d / rapier3d

Physics simulation is handled by Rapier.

Responsibilities:

- Collision detection
- Rigid body simulation
- Character controller
- Ray casting
- Continuous collision detection
- Trigger volumes
- Joint constraints

Both 2D and 3D physics use the same architecture.

------------------------------------------------------------------------

## Audio

### kira

The audio subsystem manages all sound playback.

Responsibilities:

- Music
- Sound effects
- Spatial audio
- Audio mixing
- Volume control
- Streaming

------------------------------------------------------------------------

## User Interface

### egui

The editor interface is built using immediate-mode GUI.

Responsibilities:

- Hierarchy
- Inspector
- Asset browser
- Console
- Profiler
- Material editor
- Animation editor

This UI is intended for tools only, not in-game UI.

------------------------------------------------------------------------

## Asset Management

Assets are loaded asynchronously and referenced by unique identifiers.

Supported asset types include:

- Meshes
- Textures
- Materials
- Audio
- Animations
- Scenes
- Shaders

Features:

- Asset caching
- Reference counting
- Dependency tracking
- Hot reload
- Background loading

------------------------------------------------------------------------

## Model Format

### glTF 2.0

glTF is used as the standard 3D asset format.

Supported features:

- Meshes
- Skeletons
- Animations
- PBR materials
- Cameras

------------------------------------------------------------------------

## Serialization

### serde + RON

Scenes, prefabs, and project settings are stored in human-readable
files.

Benefits:

- Easy version control
- Manual editing
- Fast loading
- Extensible

------------------------------------------------------------------------

## Parallel Processing

### rayon

Many CPU-intensive operations benefit from multithreading.

Examples:

- Asset importing
- Mesh processing
- Terrain generation
- Visibility calculations

------------------------------------------------------------------------

## Logging

### tracing

Provides structured logs for debugging and profiling.

Supports:

- Engine logs
- Editor logs
- Runtime diagnostics
- Performance tracing

------------------------------------------------------------------------

## File Watching

### notify

Automatically detects changes to files during development.

Examples:

- Shader recompilation
- Texture updates
- Scene reload
- Material changes

------------------------------------------------------------------------

## Async Runtime

### tokio

Handles background tasks without blocking the main game loop.

Typical usage:

- Asset loading
- Networking
- AI services
- Editor background jobs

------------------------------------------------------------------------

## Networking

### renet

Provides multiplayer networking.

Future capabilities:

- Client/server architecture
- Synchronization
- RPC
- Prediction

------------------------------------------------------------------------

# Engine Directory Structure

    engine/
    │
    ├── core/
    ├── platform/
    ├── renderer/
    ├── ecs/
    ├── scene/
    ├── asset/
    ├── physics/
    ├── animation/
    ├── audio/
    ├── ui/
    ├── editor/
    ├── scripting/
    ├── network/
    ├── ai/
    └── utils/

    game/

## core/

The foundation of the engine.

Responsibilities:

- Engine startup
- Main loop
- Configuration
- Timing
- Job system
- Global services

Everything eventually depends on the Core module.

------------------------------------------------------------------------

## platform/

Communicates with the operating system.

Responsibilities:

- Window creation
- Input
- Clipboard
- Monitor detection
- Cursor
- Native dialogs

------------------------------------------------------------------------

## renderer/

Responsible for every rendered pixel.

Submodules:

    renderer/
    ├── device/
    ├── pipeline/
    ├── material/
    ├── texture/
    ├── mesh/
    ├── lighting/
    ├── shadow/
    ├── postprocess/
    ├── skybox/
    └── debug/

Responsibilities:

- Rendering
- GPU resources
- Lighting
- Shadows
- Post-processing
- Debug rendering

------------------------------------------------------------------------

## ecs/

Contains all entity management.

Responsibilities:

- Components
- Systems
- Events
- Resources
- Scheduling

------------------------------------------------------------------------

## scene/

Manages the game world.

Responsibilities:

- Scene loading
- Scene saving
- Prefabs
- Hierarchy
- Scene switching

------------------------------------------------------------------------

## asset/

Responsible for importing and caching resources.

Responsibilities:

- Import pipeline
- Asset database
- UUID generation
- Background loading
- Asset references

------------------------------------------------------------------------

## physics/

Handles simulation.

Responsibilities:

- World simulation
- Character controller
- Collision
- Physics debug rendering

------------------------------------------------------------------------

## animation/

Responsible for animated objects.

Responsibilities:

- Skeletons
- Blend trees
- Animation state machine
- Animation events

------------------------------------------------------------------------

## audio/

All sound systems.

Responsibilities:

- Music
- Sound effects
- Spatial audio
- Audio mixer

------------------------------------------------------------------------

## ui/

Runtime user interface.

Future features:

- HUD
- Buttons
- Text
- Images
- Layout

------------------------------------------------------------------------

## editor/

Contains all development tools.

    editor/
    ├── hierarchy/
    ├── inspector/
    ├── assets/
    ├── scene_view/
    ├── game_view/
    ├── console/
    ├── profiler/
    ├── animation/
    └── material/

------------------------------------------------------------------------

## scripting/

Contains interfaces allowing gameplay code to interact with engine
systems.

Although gameplay is written entirely in Rust, this module exposes
engine APIs in a clean and organized manner.

------------------------------------------------------------------------

## network/

Future multiplayer support.

------------------------------------------------------------------------

## ai/

AI integration layer.

Responsibilities:

- MCP server
- AI commands
- Code generation
- Scene manipulation
- Asset generation
- Editor automation

------------------------------------------------------------------------

## utils/

Shared helper utilities.

Examples:

- File helpers
- Math helpers
- UUID
- Hashing
- Serialization helpers

------------------------------------------------------------------------

# Game Project Structure

    game/
    ├── src/
    ├── assets/
    ├── scenes/
    ├── prefabs/
    ├── materials/
    ├── shaders/
    ├── textures/
    ├── models/
    ├── audio/
    └── config/

This directory contains only game-specific content. The engine itself
remains reusable across multiple projects.

------------------------------------------------------------------------

# Development Roadmap

The project should be developed in incremental milestones. Each
milestone results in a working engine before moving to the next.

## Milestone 1 — Engine Foundation

Goal:

Create a window displaying a clear color.

Tasks:

- Create Cargo workspace
- Initialize engine crate
- Integrate winit
- Initialize wgpu
- Render first frame
- Handle resize
- Handle input
- Implement logging
- Create application loop

Deliverable:

A running application with a GPU-backed rendering window.

------------------------------------------------------------------------

## Milestone 2 — Renderer Core

Goal:

Render basic geometry.

Tasks:

- Camera
- Transform
- Mesh abstraction
- Vertex buffers
- Index buffers
- Uniform buffers
- Basic shaders
- Texture loading

Deliverable:

Render textured cubes.

------------------------------------------------------------------------

## Milestone 3 — ECS Integration

Goal:

Everything becomes an entity.

Tasks:

- Integrate bevy_ecs
- Entity creation
- Components
- Systems
- Scheduler
- Resources

Deliverable:

Spawn entities dynamically.

------------------------------------------------------------------------

## Milestone 4 — Scene System

Goal:

Persistent game worlds.

Tasks:

- Scene serialization
- Scene loading
- Scene saving
- Entity hierarchy
- Prefab system

Deliverable:

Open and save scenes.

------------------------------------------------------------------------

## Milestone 5 — Asset Pipeline

Goal:

Professional asset workflow.

Tasks:

- Asset database
- glTF importer
- Texture importer
- Audio importer
- UUID management
- Hot reload

Deliverable:

Automatically import assets.

------------------------------------------------------------------------

## Milestone 6 — PBR Renderer

Goal:

Modern graphics.

Tasks:

- Directional lights
- Point lights
- Shadows
- Skybox
- PBR materials
- HDR

Deliverable:

A visually correct 3D scene.

------------------------------------------------------------------------

## Milestone 7 — Physics

Goal:

Interactive gameplay.

Tasks:

- Rapier integration
- Collision
- Rigidbody
- Character controller
- Ray casting

Deliverable:

Walkable 3D world with collision.

------------------------------------------------------------------------

## Milestone 8 — Audio

Goal:

Complete audiovisual experience.

Tasks:

- Music
- Sound effects
- Spatial sound
- Audio mixer

Deliverable:

3D positional audio.

------------------------------------------------------------------------

## Milestone 9 — Editor

Goal:

Visual development environment.

Tasks:

- Scene View
- Game View
- Inspector
- Hierarchy
- Asset Browser
- Console
- Gizmos

Deliverable:

A usable editor for creating levels.

------------------------------------------------------------------------

## Milestone 10 — Animation

Goal:

Animated characters.

Tasks:

- Skeleton importer
- Animation playback
- Blend trees
- State machines

Deliverable:

Animated characters using glTF.

------------------------------------------------------------------------

## Milestone 11 — Runtime UI

Goal:

Support game interfaces.

Tasks:

- Text rendering
- Buttons
- Images
- Layout system

Deliverable:

HUD and menu support.

------------------------------------------------------------------------

## Milestone 12 — AI Integration

Goal:

AI-assisted game development.

Tasks:

- MCP server
- Editor commands
- Scene generation
- Entity manipulation
- Asset creation
- Automated workflows

Deliverable:

AI agents can directly interact with the engine and editor.

------------------------------------------------------------------------

# First Complete Game Target

After completing Milestone 12, the engine should be capable of creating
a simple third-person or first-person 3D game with the following
features:

- Window creation
- Cross-platform builds
- ECS architecture
- Scene management
- Asset pipeline
- glTF model loading
- PBR rendering
- Lighting and shadows
- Physics simulation
- Character controller
- Audio playback
- Animation
- Runtime UI
- Visual editor
- AI-assisted development
- Build and packaging pipeline for Windows, macOS, and Linux

This milestone represents the first stable release capable of supporting
complete game development while providing a solid architectural
foundation for future expansion into networking, advanced rendering,
terrain systems, visual scripting, and other advanced engine features.
