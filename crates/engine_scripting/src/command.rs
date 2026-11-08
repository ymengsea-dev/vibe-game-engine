//! Engine-owned gameplay commands shared by Lua and Rust games.

/// The game state names, in the order `engine_scene::GameStateName::ALL` lists
/// them (`engine` tests that the two agree).
pub const GAME_STATES: [&str; 4] = ["menu", "playing", "paused", "game_over"];

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
    /// Set a scene object's local rotation.
    SetRotation {
        /// Stable scene object ID.
        object_id: String,
        /// Finite unit quaternion (xyzw).
        rotation: [f32; 4],
    },
    /// Rotate a scene object by a delta in its local space (`current * delta`).
    Rotate {
        /// Stable scene object ID.
        object_id: String,
        /// Finite unit quaternion (xyzw).
        delta: [f32; 4],
    },
    /// Ask the host to lock (and hide) or release the mouse cursor.
    SetMouseLocked {
        /// Whether the game wants the cursor locked.
        locked: bool,
    },
    /// Set a scene object's local scale.
    SetScale {
        /// Stable scene object ID.
        object_id: String,
        /// Finite, non-zero scale on every axis.
        scale: [f32; 3],
    },
    /// Set a dynamic body's linear velocity (world units per second).
    SetVelocity {
        /// Stable scene object ID.
        object_id: String,
        /// Finite velocity.
        velocity: [f32; 3],
    },
    /// Push a dynamic body with a force for `duration` seconds
    /// (an impulse of `force * duration`).
    AddForce {
        /// Stable scene object ID.
        object_id: String,
        /// Finite force.
        force: [f32; 3],
        /// Finite seconds, at least zero, the force acts for.
        duration: f32,
    },
    /// Give a dynamic body an instant change of momentum.
    AddImpulse {
        /// Stable scene object ID.
        object_id: String,
        /// Finite impulse.
        impulse: [f32; 3],
    },
    /// Move a kinematic character body by `delta`, sliding along obstacles.
    MoveCharacter {
        /// Stable scene object ID.
        object_id: String,
        /// Finite desired translation for this step.
        delta: [f32; 3],
    },
    /// Make a scene camera object the one the game views through.
    SetCamera {
        /// Stable scene object ID of a camera.
        object_id: String,
    },
    /// Point a follow camera at another object (resets its follow state).
    SetFollowTarget {
        /// Stable scene object ID of the follow camera.
        camera_id: String,
        /// Stable scene object ID of the object to follow.
        target_id: String,
    },
    /// Remove a scene object and everything parented under it.
    Destroy {
        /// Stable scene object ID.
        object_id: String,
    },
    /// Show or hide a scene object and its descendants (hidden objects are not
    /// drawn but keep simulating).
    SetVisible {
        /// Stable scene object ID.
        object_id: String,
        /// Whether the object should be drawn.
        visible: bool,
    },
    /// Enable or disable a scene object and its descendants (disabled objects
    /// are not drawn and their physics is switched off).
    SetEnabled {
        /// Stable scene object ID.
        object_id: String,
        /// Whether the object should take part in the game.
        enabled: bool,
    },
    /// Shake the presented view; a new shake replaces the current one.
    CameraShake {
        /// Largest offset in world units; finite and at least zero.
        strength: f32,
        /// Duration in seconds, `0..=10`.
        seconds: f32,
    },
    /// Switch the game state (one of [`GAME_STATES`]); `paused` freezes the
    /// simulation.
    SetGameState {
        /// A name from [`GAME_STATES`].
        state: String,
    },
}
