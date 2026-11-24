//! Engine-owned gameplay commands shared by Lua and Rust games.

/// The game state names, in the order `engine_scene::GameStateName::ALL` lists
/// them (`engine` tests that the two agree).
pub const GAME_STATES: [&str; 4] = ["menu", "playing", "paused", "game_over"];

/// The graphics quality names, lowest first, in the order
/// `engine_renderer::QualityLevel::ALL` lists them (`engine` tests that the
/// two agree).
pub const QUALITY_LEVELS: [&str; 3] = ["low", "medium", "high"];

/// The Simple AI brain's state names, in the order
/// `engine_scene::AiState::ALL` lists them (`engine` tests that the two agree).
pub const AI_STATES: [&str; 4] = ["idle", "patrol", "chase", "attack"];

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
    /// Pause or resume an authored sprite-sheet animation or an animated
    /// model's clip.
    SetAnimationPlaying {
        /// Stable scene object ID of the animated sprite or model.
        object_id: String,
        /// Whether its frame clock should advance.
        playing: bool,
    },
    /// Play one of an animated model's or sprite's clips from its start.
    PlayAnimation {
        /// Stable scene object ID of the animated model or sprite.
        object_id: String,
        /// The clip's name in the model file or sprite (1 to 256 bytes).
        clip: String,
        /// Whether the clip starts again after its last frame; `None` keeps
        /// the clip's own setting (a model's clips loop).
        looping: Option<bool>,
        /// Playback rate, finite and within `-100..=100`; `1` is authored speed.
        speed: f32,
        /// Seconds to blend from the clip playing now, `0..=10`; `0` switches at once.
        fade: f32,
    },
    /// Set a number one object's animator reads before the game's shared
    /// variables (such as its own `speed`).
    SetAnimatorValue {
        /// Stable scene object ID of the animated model.
        object_id: String,
        /// Printable name, 1 to 64 bytes.
        name: String,
        /// Finite value.
        value: f64,
    },
    /// Change how fast an animated model's clip plays.
    SetAnimationSpeed {
        /// Stable scene object ID of the animated model.
        object_id: String,
        /// Playback rate, finite and within `-100..=100`.
        speed: f32,
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
    /// Load a scene on top of the current one; everything already there stays.
    LoadScene {
        /// Project-relative `scenes/*.ron` path.
        path: String,
    },
    /// Remove a scene that was loaded on top, and its objects.
    UnloadScene {
        /// Project-relative `scenes/*.ron` path.
        path: String,
    },
    /// Change to the scene after this one in the project's scene order.
    NextScene,
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
    /// Switch graphics quality: shadow sharpness, mesh detail and draw
    /// distance.
    SetQuality {
        /// One of [`QUALITY_LEVELS`].
        level: String,
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
    /// Walk a scene object to a world position along the scene's nav grid
    /// (straight there when the scene has none), with its AI settings, or
    /// default ones.
    MoveTo {
        /// Stable scene object ID.
        object_id: String,
        /// Finite world position to walk to.
        target: [f32; 3],
        /// Finite walking speed, or the object's own when `None`.
        speed: Option<f32>,
    },
    /// Stop a scene object walking where `MoveTo` sent it.
    StopMoving {
        /// Stable scene object ID.
        object_id: String,
    },
    /// Make a noise that AI within its radius (and their hearing) notices.
    MakeNoise {
        /// Finite world position of the noise.
        position: [f32; 3],
        /// Finite distance it carries, at least zero.
        radius: f32,
    },
    /// Put a scene object with a Simple AI brain into one of its states.
    SetAiState {
        /// Stable scene object ID.
        object_id: String,
        /// One of [`AI_STATES`].
        state: String,
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
