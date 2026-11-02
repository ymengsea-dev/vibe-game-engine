//! Engine-owned gameplay commands shared by Lua and Rust games.

/// A validated gameplay request for the engine to apply.
///
/// Lua scripts produce these through `engine.*` calls; Rust games construct
/// them directly. `engine::gameplay::GameplayRuntime::apply` is the single
/// place that turns them into world, physics, UI, audio and scene changes.
#[derive(Debug, Clone, PartialEq)]
pub enum GameCommand {
    /// Append a message to the game's output.
    Log(String),
    /// Persist the Lua game's bounded values at the end of this frame.
    RequestSave,
    /// Trigger an authored audio emitter on a scene object.
    PlayAudio {
        /// Stable scene object ID of the emitter.
        object_id: String,
    },
    /// Pause or resume an authored sprite-sheet animation.
    SetAnimationPlaying {
        /// Stable scene object ID of the animated sprite.
        object_id: String,
        /// Whether its frame clock should advance.
        playing: bool,
    },
    /// Change the visible caption of an authored label or button.
    UiSetText {
        /// Stable scene object ID of the UI element.
        object_id: String,
        /// Replacement caption (at most 1024 bytes).
        text: String,
    },
    /// Set a numeric gameplay variable shared with visual rules.
    SetVariable {
        /// Printable variable name (at most 64 bytes).
        name: String,
        /// Finite new value.
        value: f64,
    },
    /// Send a named event to authored visual rules.
    EmitEvent {
        /// Printable event name (at most 64 bytes).
        name: String,
    },
    /// Replace the current authored scene with another packaged scene.
    ChangeScene {
        /// Project-relative path under `scenes/`.
        path: String,
    },
    /// Spawn an empty named scene object with a finite parent-relative position.
    SpawnEmpty {
        /// Stable object ID allocated by the Lua runtime.
        object_id: String,
        /// Display name of the new object.
        name: String,
        /// Parent-relative translation.
        position: [f32; 3],
    },
    /// Spawn a packaged single-object prefab at a finite parent-relative position.
    SpawnPrefab {
        /// Stable object ID allocated by the Lua runtime.
        object_id: String,
        /// Project-relative path under `assets/prefabs`.
        path: String,
        /// Parent-relative translation.
        position: [f32; 3],
    },
    /// Move a scene object to a finite parent-relative position.
    SetPosition {
        /// Stable scene object ID.
        object_id: String,
        /// Parent-relative translation.
        position: [f32; 3],
    },
    /// Move a scene object by a finite parent-relative delta.
    Translate {
        /// Stable scene object ID.
        object_id: String,
        /// Parent-relative displacement.
        delta: [f32; 3],
    },
}
