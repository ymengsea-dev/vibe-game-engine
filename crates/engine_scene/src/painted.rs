//! Painted object groups: a parent object whose children were painted onto
//! a terrain with Studio's object brush (rocks, props). The children are
//! ordinary objects; the group only remembers which terrain they stand on
//! and what was painted, so the brush can erase them and keep them on the
//! ground after sculpting.

use serde::{Deserialize, Serialize};

use crate::format::AssetRef;

/// What a painted group was painted with, saved on the group object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaintedGroupData {
    /// Object ID of the terrain the copies stand on.
    pub terrain: String,
    /// The model or prefab painted.
    pub source: AssetRef,
}

impl PaintedGroupData {
    /// Checks the terrain link and source.
    ///
    /// # Errors
    ///
    /// A short reason when a field is empty.
    pub fn validate(&self) -> Result<(), String> {
        if self.terrain.is_empty() {
            return Err("terrain link must not be empty".to_owned());
        }
        if self.source.id.is_empty() {
            return Err("source must not be empty".to_owned());
        }
        Ok(())
    }
}

/// The live copy of an object's [`PaintedGroupData`].
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct ScenePaintedGroup(pub PaintedGroupData);
