//! Low / Medium / High graphics quality and what each level changes.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::shadow::SHADOW_MAP_SIZE;

/// A game's graphics quality. Saved in the project and switchable while
/// playing (an options menu, say).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum QualityLevel {
    /// Small shadows, simpler meshes sooner, shorter draw distance.
    Low,
    /// The balanced default.
    #[default]
    Medium,
    /// Sharp shadows, detailed meshes for longer, longer draw distance.
    High,
}

impl QualityLevel {
    /// Every level, lowest first.
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    /// The lower-case name scripts use: `"low"`, `"medium"`, `"high"`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// The level called `name` (any case), if any.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|level| level.name().eq_ignore_ascii_case(name))
    }

    /// What this level changes.
    pub fn preset(self) -> QualityPreset {
        match self {
            Self::Low => QualityPreset {
                shadow_map_size: SHADOW_MAP_SIZE / 2,
                lod_bias: 1.5,
                draw_distance_scale: 0.6,
            },
            Self::Medium => QualityPreset {
                shadow_map_size: SHADOW_MAP_SIZE,
                lod_bias: 1.0,
                draw_distance_scale: 1.0,
            },
            Self::High => QualityPreset {
                shadow_map_size: SHADOW_MAP_SIZE * 2,
                lod_bias: 0.75,
                draw_distance_scale: 1.5,
            },
        }
    }
}

/// The settings one [`QualityLevel`] stands for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityPreset {
    /// Width and height of the sun's shadow map, in texels.
    pub shadow_map_size: u32,
    /// Multiplies camera distance before picking a mesh's detail tier:
    /// above 1 switches to simpler meshes sooner.
    pub lod_bias: f32,
    /// Multiplies every object's draw distance.
    pub draw_distance_scale: f32,
}

/// Where the camera is and which quality applies, for picking mesh tiers
/// and draw distances while extracting a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistanceQuality {
    /// The viewing camera's position in world space.
    pub eye: Vec3,
    /// The active quality.
    pub preset: QualityPreset,
}

impl DistanceQuality {
    /// Distance settings for a camera at `eye` drawing at `level`.
    pub fn new(eye: Vec3, level: QualityLevel) -> Self {
        Self {
            eye,
            preset: level.preset(),
        }
    }

    /// Camera distance to `point`, scaled by the LOD bias, for picking a
    /// detail tier.
    pub fn lod_distance(&self, point: Vec3) -> f32 {
        self.eye.distance(point) * self.preset.lod_bias
    }

    /// `draw_distance` scaled by the active quality.
    pub fn draw_distance(&self, draw_distance: f32) -> f32 {
        draw_distance * self.preset.draw_distance_scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for level in QualityLevel::ALL {
            assert_eq!(QualityLevel::from_name(level.name()), Some(level));
        }
        assert_eq!(QualityLevel::from_name("HIGH"), Some(QualityLevel::High));
        assert_eq!(QualityLevel::from_name("ultra"), None);
    }

    #[test]
    fn medium_keeps_todays_look() {
        let medium = QualityLevel::default().preset();
        assert_eq!(medium.shadow_map_size, SHADOW_MAP_SIZE);
        assert_eq!(medium.lod_bias, 1.0);
        assert_eq!(medium.draw_distance_scale, 1.0);
    }

    #[test]
    fn higher_levels_draw_more() {
        let [low, medium, high] = QualityLevel::ALL.map(QualityLevel::preset);
        assert!(low.shadow_map_size < medium.shadow_map_size);
        assert!(medium.shadow_map_size < high.shadow_map_size);
        assert!(low.lod_bias > high.lod_bias);
        assert!(low.draw_distance_scale < high.draw_distance_scale);
    }

    #[test]
    fn distances_use_the_preset() {
        let view = DistanceQuality::new(Vec3::ZERO, QualityLevel::Low);
        assert_eq!(view.lod_distance(Vec3::new(0.0, 0.0, 10.0)), 15.0);
        assert!((view.draw_distance(100.0) - 60.0).abs() < 1e-4);
    }
}
