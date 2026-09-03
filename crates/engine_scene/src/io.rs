//! File I/O for scenes: save/load RON files, validating on both ends.

use std::path::Path;

use crate::error::SceneError;
use crate::format::Scene;

/// Writes `contents` to `path` atomically: write a sibling temp file,
/// then rename it over `path`. A crash mid-write leaves the previous
/// file intact rather than a half-written one. The parent directory is
/// created if missing.
///
/// # Errors
///
/// Any underlying [`std::io`] failure (create dir, write temp, rename).
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scene".to_string());
    let tmp = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, contents)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            Err(err)
        }
    }
}

impl Scene {
    /// Validates every entity in this scene, and the parent/child
    /// hierarchy formed by their [`crate::SceneEntity::parent`] indices
    /// (in bounds, acyclic).
    ///
    /// # Errors
    ///
    /// Returns [`SceneError::Validation`] naming the first invalid entity
    /// (by index) and why.
    pub fn validate(&self) -> Result<(), SceneError> {
        for (index, entity) in self.entities.iter().enumerate() {
            entity
                .validate()
                .map_err(|reason| SceneError::Validation(format!("entity {index}: {reason}")))?;
        }
        self.validate_hierarchy().map_err(SceneError::Validation)?;
        Ok(())
    }

    /// Checks that every [`crate::SceneEntity::parent`] index is in
    /// bounds, isn't self-referential, and that no chain of parents forms
    /// a cycle.
    ///
    /// Runs in O(entities) total: a three-state (unvisited/visiting/done)
    /// walk marks each entity at most once, so no ancestor chain is
    /// re-walked from scratch for entities that share it — not the O(n²)
    /// a naive "walk up from every entity" would cost on a scene where
    /// many entities share a deep hierarchy.
    fn validate_hierarchy(&self) -> Result<(), String> {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum State {
            Unvisited,
            Visiting,
            Done,
        }

        let len = self.entities.len();
        for (index, entity) in self.entities.iter().enumerate() {
            if let Some(parent) = entity.parent {
                if parent >= len {
                    return Err(format!(
                        "entity {index}: parent index {parent} out of bounds ({len} entities)"
                    ));
                }
                if parent == index {
                    return Err(format!("entity {index}: cannot be its own parent"));
                }
            }
        }

        let mut state = vec![State::Unvisited; len];
        for start in 0..len {
            if state[start] != State::Unvisited {
                continue;
            }
            let mut path = Vec::new();
            let mut current = start;
            loop {
                match state[current] {
                    State::Done => break,
                    State::Visiting => {
                        return Err(format!("entity {current}: parent chain contains a cycle"));
                    }
                    State::Unvisited => {
                        state[current] = State::Visiting;
                        path.push(current);
                        match self.entities[current].parent {
                            Some(parent) => current = parent,
                            None => break,
                        }
                    }
                }
            }
            for node in path {
                state[node] = State::Done;
            }
        }
        Ok(())
    }

    /// Validates and writes this scene to `path` as RON text, overwriting
    /// any existing file.
    ///
    /// # Errors
    ///
    /// Returns [`SceneError::Validation`] if this scene fails validation
    /// (see [`Scene::validate`]), [`SceneError::Serialize`] if RON
    /// encoding fails, or [`SceneError::Io`] if the write fails.
    pub fn save_to_file(&self, path: &Path) -> Result<(), SceneError> {
        self.validate()?;
        let text = self.to_ron_string()?;
        write_atomic(path, text.as_bytes()).map_err(|err| SceneError::Io(err.to_string()))
    }

    /// Reads, parses, and validates a scene from `path`.
    ///
    /// The file's contents are untrusted input — this rejects malformed
    /// RON ([`SceneError::Deserialize`]) and semantically invalid data
    /// ([`SceneError::Validation`]) rather than propagating either as a
    /// panic or as silently-corrupt in-memory state.
    ///
    /// # Errors
    ///
    /// Returns [`SceneError::Io`] if the file can't be read,
    /// [`SceneError::Deserialize`] if its contents aren't valid RON
    /// matching [`Scene`]'s shape, or [`SceneError::Validation`] if the
    /// parsed scene fails validation.
    pub fn load_from_file(path: &Path) -> Result<Self, SceneError> {
        let text = std::fs::read_to_string(path).map_err(|err| SceneError::Io(err.to_string()))?;
        let scene = Self::from_ron_str(&text)?;
        scene.validate()?;
        Ok(scene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{CameraData, ProjectionData, SceneEntity, TransformData};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Unique path per test in the OS temp dir — avoids needing a
    /// `tempfile`-style dependency for what's otherwise a couple of
    /// `std::fs` calls.
    fn temp_scene_path(name: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "vge-engine_scene-test-{name}-{}-{n}.ron",
            std::process::id()
        ))
    }

    fn valid_scene() -> Scene {
        Scene {
            entities: vec![SceneEntity {
                transform: Some(TransformData {
                    translation: [1.0, 2.0, 3.0],
                    rotation: glam::Quat::IDENTITY.to_array(),
                    scale: [1.0, 1.0, 1.0],
                }),
                camera: Some(CameraData {
                    eye: [0.0, 0.0, 5.0],
                    target: [0.0, 0.0, 0.0],
                    up: [0.0, 1.0, 0.0],
                    aspect_ratio: 16.0 / 9.0,
                    projection: ProjectionData::Perspective {
                        fov_y_radians: std::f32::consts::FRAC_PI_4,
                    },
                    near: 0.1,
                    far: 100.0,
                }),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn validate_passes_for_default_scene() {
        assert!(Scene::default().validate().is_ok());
    }

    #[test]
    fn validate_reports_entity_index() {
        let mut scene = valid_scene();
        scene.entities.push(SceneEntity {
            transform: Some(TransformData {
                translation: [f32::NAN, 0.0, 0.0],
                rotation: glam::Quat::IDENTITY.to_array(),
                scale: [1.0, 1.0, 1.0],
            }),
            ..Default::default()
        });
        let err = scene.validate().unwrap_err();
        let SceneError::Validation(message) = err else {
            panic!("expected Validation error, got {err:?}");
        };
        assert!(message.contains("entity 1"));
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_scene_path("round-trip");
        let scene = valid_scene();

        scene.save_to_file(&path).unwrap();
        let loaded = Scene::load_from_file(&path).unwrap();

        std::fs::remove_file(&path).ok();
        assert_eq!(loaded, scene);
    }

    #[test]
    fn save_rejects_invalid_scene_without_touching_disk() {
        let path = temp_scene_path("save-rejects-invalid");
        let mut scene = valid_scene();
        scene.entities[0].camera.as_mut().unwrap().near = -1.0;

        let err = scene.save_to_file(&path).unwrap_err();
        assert!(matches!(err, SceneError::Validation(_)));
        assert!(!path.exists());
    }

    #[test]
    fn load_missing_file_returns_io_error() {
        let path = temp_scene_path("does-not-exist");
        let err = Scene::load_from_file(&path).unwrap_err();
        assert!(matches!(err, SceneError::Io(_)));
    }

    #[test]
    fn load_rejects_malformed_ron_file() {
        let path = temp_scene_path("malformed");
        std::fs::write(&path, "not valid ron {{{").unwrap();

        let err = Scene::load_from_file(&path).unwrap_err();
        std::fs::remove_file(&path).ok();
        assert!(matches!(err, SceneError::Deserialize(_)));
    }

    #[test]
    fn load_rejects_semantically_invalid_scene_file() {
        let path = temp_scene_path("invalid-semantics");
        let mut scene = valid_scene();
        // Bypass `save_to_file`'s own validation to write an invalid
        // scene directly, simulating a hand-corrupted file.
        scene.entities[0].transform.as_mut().unwrap().rotation = [0.0, 0.0, 0.0, 5.0];
        std::fs::write(&path, scene.to_ron_string().unwrap()).unwrap();

        let err = Scene::load_from_file(&path).unwrap_err();
        std::fs::remove_file(&path).ok();
        assert!(matches!(err, SceneError::Validation(_)));
    }

    fn root_and_child() -> Scene {
        Scene {
            entities: vec![
                SceneEntity {
                    name: Some("Root".to_string()),
                    ..Default::default()
                },
                SceneEntity {
                    name: Some("Child".to_string()),
                    parent: Some(0),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn hierarchy_with_valid_parent_passes() {
        assert!(root_and_child().validate().is_ok());
    }

    #[test]
    fn hierarchy_with_out_of_bounds_parent_fails() {
        let mut scene = root_and_child();
        scene.entities[1].parent = Some(99);
        let err = scene.validate().unwrap_err();
        let SceneError::Validation(message) = err else {
            panic!("expected Validation error, got {err:?}");
        };
        assert!(message.contains("out of bounds"));
    }

    #[test]
    fn hierarchy_with_self_parent_fails() {
        let mut scene = root_and_child();
        scene.entities[0].parent = Some(0);
        let err = scene.validate().unwrap_err();
        let SceneError::Validation(message) = err else {
            panic!("expected Validation error, got {err:?}");
        };
        assert!(message.contains("own parent"));
    }

    #[test]
    fn hierarchy_with_two_entity_cycle_fails() {
        let mut scene = root_and_child();
        // 0 -> 1 -> 0
        scene.entities[0].parent = Some(1);
        let err = scene.validate().unwrap_err();
        let SceneError::Validation(message) = err else {
            panic!("expected Validation error, got {err:?}");
        };
        assert!(message.contains("cycle"));
    }

    #[test]
    fn hierarchy_with_three_level_chain_passes() {
        let mut scene = root_and_child();
        scene.entities.push(SceneEntity {
            name: Some("Grandchild".to_string()),
            parent: Some(1),
            ..Default::default()
        });
        assert!(scene.validate().is_ok());
    }

    #[test]
    fn hierarchy_with_shared_ancestor_validates_each_branch_once() {
        // Two children of the same root — proves the "done" state short-
        // circuits re-walking an already-verified ancestor chain rather
        // than erroring on revisiting entity 0.
        let mut scene = root_and_child();
        scene.entities.push(SceneEntity {
            name: Some("Child2".to_string()),
            parent: Some(0),
            ..Default::default()
        });
        assert!(scene.validate().is_ok());
    }
}
