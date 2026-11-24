//! World areas: parts of a big world kept in their own scene files and
//! loaded on top of the main scene only while the camera is near.
//!
//! An area is an object in the main scene with a [`StreamAreaData`]: a box
//! around the object and the scene to load. `engine::gameplay` streams them;
//! this module is the format, its checks and the pure choice of what to load
//! and unload ([`plan_streaming`]), so Studio and tests can share it.

use std::collections::{BTreeMap, HashSet};

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// The largest size or load distance an area may have, in world units.
pub const MAX_AREA_DISTANCE: f32 = 100_000.0;
/// How much farther than its load distance the camera must go before an area
/// unloads, as a share of that distance, so standing at the edge doesn't
/// load and unload it every frame.
const UNLOAD_MARGIN_SHARE: f32 = 0.1;
/// The smallest unload margin, in world units.
const MIN_UNLOAD_MARGIN: f32 = 2.0;

/// One world area, saved on its object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamAreaData {
    /// The scene loaded while the camera is near (`scenes/forest.ron`).
    /// Empty while the area isn't set up yet; it then loads nothing.
    #[serde(default)]
    pub scene: String,
    /// Half the box's size on each axis, in world units. The box is centred
    /// on the object and doesn't turn with it.
    #[serde(default = "default_half_extents")]
    pub half_extents: [f32; 3],
    /// How close to the box the camera must come for the scene to load.
    #[serde(default = "default_load_distance")]
    pub load_distance: f32,
}

fn default_half_extents() -> [f32; 3] {
    [32.0, 8.0, 32.0]
}

fn default_load_distance() -> f32 {
    30.0
}

impl Default for StreamAreaData {
    fn default() -> Self {
        Self {
            scene: String::new(),
            half_extents: default_half_extents(),
            load_distance: default_load_distance(),
        }
    }
}

impl StreamAreaData {
    /// Checks the scene path, the size and the load distance.
    ///
    /// # Errors
    ///
    /// A short reason when a value is out of range.
    pub fn validate(&self) -> Result<(), String> {
        if !self.scene.is_empty() && !crate::valid_scene_path(&self.scene) {
            return Err(format!("scene {:?} is not a scene path", self.scene));
        }
        if !self
            .half_extents
            .iter()
            .all(|half| half.is_finite() && *half > 0.0 && *half <= MAX_AREA_DISTANCE)
        {
            return Err("size must be above 0 on every axis".to_owned());
        }
        if !(self.load_distance.is_finite()
            && (0.0..=MAX_AREA_DISTANCE).contains(&self.load_distance))
        {
            return Err("load distance must be 0 or more".to_owned());
        }
        Ok(())
    }

    /// How far beyond the load distance the camera must go to unload it.
    #[must_use]
    pub fn unload_distance(&self) -> f32 {
        self.load_distance + (self.load_distance * UNLOAD_MARGIN_SHARE).max(MIN_UNLOAD_MARGIN)
    }
}

/// The live copy of an object's [`StreamAreaData`].
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneStreamArea(pub StreamAreaData);

/// How far `point` is from the box at `centre` with `half_extents`: 0 inside.
#[must_use]
pub fn distance_to_area(point: Vec3, centre: Vec3, half_extents: Vec3) -> f32 {
    let outside = ((point - centre).abs() - half_extents).max(Vec3::ZERO);
    outside.length()
}

/// An area placed in the world, as [`plan_streaming`] sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedArea<'a> {
    /// What the area holds.
    pub data: &'a StreamAreaData,
    /// The object's world position, the box's centre.
    pub centre: Vec3,
}

/// What streaming should do this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamPlan {
    /// The one scene to load now, the nearest that is wanted.
    pub load: Option<String>,
    /// Scenes streaming loaded that are now too far away (or whose area is
    /// gone), in name order.
    pub unload: Vec<String>,
}

/// Chooses what to load and unload with the camera at `eye`.
///
/// `streamed` is what streaming loaded earlier. `busy` says whether a scene is
/// open for another reason (it is the current scene, or a script or rule
/// loaded it): streaming never loads those again and never unloads them.
/// Several areas may name one scene; it stays while any of them wants it.
#[must_use]
pub fn plan_streaming(
    areas: &[PlacedArea<'_>],
    eye: Vec3,
    streamed: &HashSet<String>,
    busy: impl Fn(&str) -> bool,
) -> StreamPlan {
    // Per scene: the nearest wanting area's distance, and whether any area
    // still keeps it.
    let mut wanted: BTreeMap<&str, f32> = BTreeMap::new();
    let mut kept: HashSet<&str> = HashSet::new();
    for area in areas {
        let scene = area.data.scene.as_str();
        if scene.is_empty() {
            continue;
        }
        let distance = distance_to_area(eye, area.centre, Vec3::from(area.data.half_extents));
        if distance <= area.data.unload_distance() {
            kept.insert(scene);
        }
        if distance <= area.data.load_distance {
            let nearest = wanted.entry(scene).or_insert(distance);
            *nearest = nearest.min(distance);
        }
    }
    let load = wanted
        .iter()
        .filter(|(scene, _)| !streamed.contains(**scene) && !busy(scene))
        .min_by(|a, b| a.1.total_cmp(b.1))
        .map(|(scene, _)| (*scene).to_owned());
    let mut unload: Vec<String> = streamed
        .iter()
        .filter(|scene| !kept.contains(scene.as_str()))
        .cloned()
        .collect();
    unload.sort();
    StreamPlan { load, unload }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(scene: &str, load_distance: f32) -> StreamAreaData {
        StreamAreaData {
            scene: scene.to_owned(),
            half_extents: [10.0, 5.0, 10.0],
            load_distance,
        }
    }

    #[test]
    fn distance_is_zero_inside_and_measured_to_the_nearest_face_or_corner() {
        let half = Vec3::new(10.0, 5.0, 10.0);
        assert_eq!(
            distance_to_area(Vec3::new(3.0, 1.0, -9.0), Vec3::ZERO, half),
            0.0
        );
        assert_eq!(
            distance_to_area(Vec3::new(14.0, 0.0, 0.0), Vec3::ZERO, half),
            4.0
        );
        let corner = distance_to_area(Vec3::new(13.0, 5.0, 14.0), Vec3::ZERO, half);
        assert!((corner - 5.0).abs() < 1e-5);
    }

    #[test]
    fn the_nearest_wanted_area_loads_first_and_only_one_at_a_time() {
        let near = area("scenes/near.ron", 30.0);
        let far = area("scenes/far.ron", 30.0);
        let away = area("scenes/away.ron", 30.0);
        let areas = [
            PlacedArea {
                data: &far,
                centre: Vec3::new(35.0, 0.0, 0.0),
            },
            PlacedArea {
                data: &near,
                centre: Vec3::new(-15.0, 0.0, 0.0),
            },
            PlacedArea {
                data: &away,
                centre: Vec3::new(500.0, 0.0, 0.0),
            },
        ];
        let mut streamed = HashSet::new();
        let plan = plan_streaming(&areas, Vec3::ZERO, &streamed, |_| false);
        assert_eq!(plan.load.as_deref(), Some("scenes/near.ron"));
        assert!(plan.unload.is_empty());
        streamed.insert("scenes/near.ron".to_owned());
        let plan = plan_streaming(&areas, Vec3::ZERO, &streamed, |_| false);
        assert_eq!(plan.load.as_deref(), Some("scenes/far.ron"));
    }

    #[test]
    fn an_area_unloads_only_past_its_margin() {
        let data = area("scenes/forest.ron", 30.0);
        let areas = [PlacedArea {
            data: &data,
            centre: Vec3::ZERO,
        }];
        let streamed: HashSet<String> = ["scenes/forest.ron".to_owned()].into();
        // Load distance 30 m, margin 3 m: from the box face at x = 10.
        let at = |x: f32| plan_streaming(&areas, Vec3::new(x, 0.0, 0.0), &streamed, |_| false);
        assert!(at(41.0).unload.is_empty());
        assert!(at(42.9).unload.is_empty());
        assert_eq!(at(43.5).unload, vec!["scenes/forest.ron".to_owned()]);
    }

    #[test]
    fn busy_scenes_are_left_alone_and_lost_areas_unload() {
        let data = area("scenes/forest.ron", 30.0);
        let areas = [PlacedArea {
            data: &data,
            centre: Vec3::ZERO,
        }];
        let none = HashSet::new();
        let plan = plan_streaming(&areas, Vec3::ZERO, &none, |scene| {
            scene == "scenes/forest.ron"
        });
        assert_eq!(plan.load, None);
        let streamed: HashSet<String> = ["scenes/gone.ron".to_owned()].into();
        let plan = plan_streaming(&[], Vec3::ZERO, &streamed, |_| false);
        assert_eq!(plan.unload, vec!["scenes/gone.ron".to_owned()]);
    }

    #[test]
    fn an_area_without_a_scene_loads_nothing() {
        let data = StreamAreaData::default();
        let areas = [PlacedArea {
            data: &data,
            centre: Vec3::ZERO,
        }];
        let plan = plan_streaming(&areas, Vec3::ZERO, &HashSet::new(), |_| false);
        assert_eq!(plan, StreamPlan::default());
    }

    #[test]
    fn validation_rejects_bad_paths_and_sizes() {
        assert!(StreamAreaData::default().validate().is_ok());
        assert!(area("scenes/forest.ron", 30.0).validate().is_ok());
        assert!(area("../forest.ron", 30.0).validate().is_err());
        assert!(area("scenes/forest.ron", f32::NAN).validate().is_err());
        assert!(area("scenes/forest.ron", -1.0).validate().is_err());
        let flat = StreamAreaData {
            half_extents: [10.0, 0.0, 10.0],
            ..area("scenes/forest.ron", 30.0)
        };
        assert!(flat.validate().is_err());
    }

    #[test]
    fn an_area_survives_a_save_and_load() {
        let mut scene = crate::Scene::default();
        scene.entities.push(crate::SceneEntity {
            stream_area: Some(area("scenes/forest.ron", 25.0)),
            ..crate::SceneEntity::default()
        });
        let text = scene.to_ron_string().expect("ron");
        let back = crate::Scene::from_ron_str(&text).expect("scene");
        assert_eq!(
            back.entities[0].stream_area,
            Some(area("scenes/forest.ron", 25.0))
        );
        assert!(back.validate().is_ok());
    }
}
