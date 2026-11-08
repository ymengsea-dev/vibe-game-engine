//! Read-only world queries a callback may ask while it runs.
//!
//! The host implements [`ScriptQueries`] over its physics world; the Lua
//! runtime borrows it only for the duration of one callback.

/// What a ray hit.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptRayHit {
    /// Stable scene object ID of the struck object, when it has one.
    pub object_id: Option<String>,
    /// World-space hit point.
    pub point: [f32; 3],
    /// Surface normal at the hit point.
    pub normal: [f32; 3],
    /// Distance from the ray origin.
    pub distance: f32,
}

/// Queries answered by the host while a callback runs.
pub trait ScriptQueries {
    /// Casts a ray; `direction` is non-zero and `max_distance` positive and capped.
    /// Triggers are ignored. `ignore` names an object whose collider the ray
    /// passes through; an unknown ID ignores nothing.
    fn raycast(
        &self,
        origin: [f32; 3],
        direction: [f32; 3],
        max_distance: f32,
        ignore: Option<&str>,
    ) -> Option<ScriptRayHit>;

    /// Whether the object stands on something.
    fn is_grounded(&self, object_id: &str) -> bool;
}
