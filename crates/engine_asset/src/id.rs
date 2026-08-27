//! Asset identity.

use std::fmt;

use uuid::Uuid;

/// A unique, stable identifier for an asset.
///
/// Backed by a v4 (random) UUID rather than, say, a file path — paths
/// change (renames, reorganized folders); this stays stable across those,
/// which matters once scenes/prefabs start referencing assets by ID
/// (Milestone 5 importers, later features).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetId(Uuid);

impl AssetId {
    /// Generates a new, random asset ID.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_asset::AssetId;
    ///
    /// let a = AssetId::new();
    /// let b = AssetId::new();
    /// assert_ne!(a, b);
    /// ```
    #[allow(
        clippy::new_without_default,
        reason = "Default would imply a meaningful zero value; every AssetId must be freshly random"
    )]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Wraps an existing UUID as an asset ID (e.g. one loaded from a
    /// serialized asset reference).
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<Uuid> for AssetId {
    fn from(uuid: Uuid) -> Self {
        Self::from_uuid(uuid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ids_are_unique() {
        assert_ne!(AssetId::new(), AssetId::new());
    }

    #[test]
    fn from_uuid_round_trips() {
        let uuid = Uuid::new_v4();
        assert_eq!(AssetId::from_uuid(uuid).as_uuid(), uuid);
    }

    #[test]
    fn display_matches_uuid_display() {
        let uuid = Uuid::new_v4();
        let id = AssetId::from_uuid(uuid);
        assert_eq!(id.to_string(), uuid.to_string());
    }
}
