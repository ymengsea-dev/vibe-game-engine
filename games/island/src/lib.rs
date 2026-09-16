//! **Island** — the engine's playable vertical slice, as a real
//! project rather than a program that draws a game.
//!
//! ## Two halves
//!
//! - [`world`] and [`character`] *generate* the art: terrain, props,
//!   textures, the walk cycle. They are the authoring tools.
//! - [`bake`] runs them once and writes the result into this project's
//!   `assets/` and `scenes/` folders as real `.gltf`, `.png` and `.wav`
//!   files, next to a `project.ron` the studio can open.
//!
//! The game binary (`src/main.rs`) is the third piece: it plays what is
//! in the project.
//!
//! ```sh
//! cargo run -p island --bin bake   # regenerate the assets
//! cargo run -p island              # play
//! ```
//!
//! ## Why bake at all
//!
//! Generating meshes and textures into memory at startup left this
//! project with an empty `assets/` folder: nothing for the studio's
//! asset browser to show, nothing to click, nothing to replace. Writing
//! the same generated art to disk makes the generator play the part
//! Blender plays for a hand-modelled game — the art is still procedural,
//! it just exists as files now.

pub mod bake;
pub mod character;
pub mod world;
