//! Scene format version migration.
//!
//! Every bump so far has been purely additive, which is why `migrate`
//! only stamps the version. v3 → v4 added `SceneEntity::audio_emitter`
//! and `Scene::nav_grid`, both `Option` with `#[serde(default)]`, so a
//! v3 file loads silent and with no nav grid — exactly what it was.
//! v2 → v3 added `SceneEntity::collider` the same way, so a v2 file
//! loads with no body and behaves exactly as it did before.
//!
//! Format v1 → v2 was likewise additive:
//! `SceneEntity` gained `name`/`parent`/`mesh_renderer`, all `Option` with
//! `#[serde(default)]`, so a v1 RON file already deserializes straight
//! into today's `SceneEntity`/`Scene` shapes with those fields `None` —
//! `migrate` only needs to stamp the version field. Kept as an explicit,
//! separate step (rather than silently trusting field-level serde
//! defaults to be "the migration") so a future version bump that *isn't*
//! purely additive — a rename, a removed field, a restructured variant —
//! has one obvious place to add real transformation logic instead of
//! reinventing where migration happens.

use crate::error::SceneError;
use crate::format::Scene;

/// The current on-disk [`Scene`] format version. Bump this — and extend
/// `migrate` with the new version's transformation — whenever
/// `Scene`/`SceneEntity`'s shape changes.
pub const CURRENT_SCENE_VERSION: u32 = 4;

/// Migrates `scene` to [`CURRENT_SCENE_VERSION`] if it was parsed from an
/// older format, stamping its `version` field. A no-op if `scene` is
/// already current.
///
/// # Errors
///
/// Returns [`SceneError::UnsupportedVersion`] if `scene.version` is newer
/// than this engine understands — safer than silently misreading a future
/// format as today's.
pub(crate) fn migrate(mut scene: Scene) -> Result<Scene, SceneError> {
    match scene.version.cmp(&CURRENT_SCENE_VERSION) {
        std::cmp::Ordering::Equal => Ok(scene),
        std::cmp::Ordering::Less => {
            scene.version = CURRENT_SCENE_VERSION;
            Ok(scene)
        }
        std::cmp::Ordering::Greater => Err(SceneError::UnsupportedVersion {
            found: scene.version,
            max: CURRENT_SCENE_VERSION,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SceneEntity;

    #[test]
    fn current_version_scene_is_unchanged() {
        let scene = Scene {
            version: CURRENT_SCENE_VERSION,
            entities: vec![SceneEntity::default()],
            nav_grid: None,
        };
        let migrated = migrate(scene.clone()).unwrap();
        assert_eq!(migrated, scene);
    }

    #[test]
    fn older_version_is_stamped_to_current() {
        let scene = Scene {
            version: 1,
            entities: vec![SceneEntity::default()],
            nav_grid: None,
        };
        let migrated = migrate(scene).unwrap();
        assert_eq!(migrated.version, CURRENT_SCENE_VERSION);
    }

    #[test]
    fn newer_version_is_rejected() {
        let scene = Scene {
            version: CURRENT_SCENE_VERSION + 1,
            entities: Vec::new(),
            nav_grid: None,
        };
        let err = migrate(scene).unwrap_err();
        assert!(matches!(
            err,
            SceneError::UnsupportedVersion {
                found,
                max,
            } if found == CURRENT_SCENE_VERSION + 1 && max == CURRENT_SCENE_VERSION
        ));
    }

    #[test]
    fn v1_ron_without_new_fields_parses_and_migrates() {
        // A hand-written v1-shaped scene: no `version` key, entities with
        // only the fields that existed before this feature. Proves the
        // "migration path from v1" isn't just a claim — old text really
        // does parse into today's format.
        let v1_ron = r#"(
            entities: [
                (
                    transform: Some((
                        translation: (1.0, 2.0, 3.0),
                        rotation: (0.0, 0.0, 0.0, 1.0),
                        scale: (1.0, 1.0, 1.0),
                    )),
                ),
            ],
        )"#;
        let scene = Scene::from_ron_str(v1_ron).unwrap();
        assert_eq!(scene.version, CURRENT_SCENE_VERSION);
        assert_eq!(scene.entities.len(), 1);
        assert_eq!(scene.entities[0].name, None);
        assert_eq!(scene.entities[0].parent, None);
        assert_eq!(scene.entities[0].mesh_renderer, None);
    }
}
