//! Deterministic level-of-detail selection and bounded streaming requests.

use std::collections::{HashSet, VecDeque};

/// One LOD tier. Tiers should be ordered from highest to lowest detail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LodLevel {
    /// Maximum camera distance at which this tier is selected.
    pub max_distance: f32,
}

/// Selects a LOD tier from camera distance without allocations.
#[derive(Debug, Clone, PartialEq)]
pub struct LodSelector {
    levels: Vec<LodLevel>,
}

/// A concrete ordered set of asset keys for one renderable's LOD tiers.
#[derive(Debug, Clone, PartialEq)]
pub struct LodSet<T> {
    /// Highest-detail key first, lowest-detail key last.
    pub levels: Vec<T>,
    /// Distance thresholds matching each level.
    pub selector: LodSelector,
}

impl<T> LodSet<T> {
    /// Creates a LOD set; returns `None` when no levels are supplied.
    pub fn new(levels: Vec<T>, thresholds: Vec<f32>) -> Option<Self> {
        if levels.is_empty() || levels.len() != thresholds.len() {
            return None;
        }
        let mut pairs: Vec<(f32, T)> = thresholds.into_iter().zip(levels).collect();
        pairs.retain(|(distance, _)| distance.is_finite() && *distance >= 0.0);
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        if pairs.is_empty() {
            return None;
        }
        let (thresholds, levels): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
        Some(Self {
            levels,
            selector: LodSelector::new(
                thresholds
                    .into_iter()
                    .map(|max_distance| LodLevel { max_distance })
                    .collect(),
            ),
        })
    }

    /// Selects the concrete key for `distance`.
    pub fn select(&self, distance: f32) -> Option<&T> {
        self.selector
            .select(distance)
            .and_then(|index| self.levels.get(index))
    }
}

impl LodSelector {
    /// Creates a selector, discarding non-finite thresholds and sorting tiers.
    pub fn new(mut levels: Vec<LodLevel>) -> Self {
        levels.retain(|level| level.max_distance.is_finite() && level.max_distance >= 0.0);
        levels.sort_by(|a, b| a.max_distance.total_cmp(&b.max_distance));
        Self { levels }
    }

    /// Returns the tier index, falling back to the lowest-detail tier.
    pub fn select(&self, distance: f32) -> Option<usize> {
        if self.levels.is_empty() {
            return None;
        }
        let distance = distance.max(0.0);
        self.levels
            .iter()
            .position(|level| distance <= level.max_distance)
            .or(Some(self.levels.len() - 1))
    }

    /// Number of valid tiers.
    pub fn len(&self) -> usize {
        self.levels.len()
    }

    /// Whether no valid tiers are configured.
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }
}

/// How far past a switch distance the camera must move, as a share of
/// that distance, before a mesh changes tier. Without it a mesh flickers
/// between two tiers while the camera rests on the line.
pub const LOD_HYSTERESIS: f32 = 0.1;

/// Distance used for a tier boundary the caller gave no value for.
const FIRST_SWITCH_DISTANCE: f32 = 15.0;

/// Where tier `index` ends: `switch[index]`, or for a missing entry twice
/// the boundary before it (so extra tiers still get used).
fn switch_distance(switch: &[f32], index: usize) -> f32 {
    match switch.get(index) {
        Some(&distance) if distance.is_finite() && distance >= 0.0 => distance,
        _ => match index.checked_sub(1) {
            Some(previous) => switch_distance(switch, previous) * 2.0,
            None => FIRST_SWITCH_DISTANCE,
        },
    }
}

/// The tier (0 = most detailed) to draw at `distance` out of `tiers`.
///
/// Tier `i` is drawn up to `switch[i]`; the last tier has no end. The
/// mesh stays on `current` until the camera is [`LOD_HYSTERESIS`] past a
/// boundary, so small camera moves don't swap meshes back and forth.
pub fn select_lod_tier(switch: &[f32], tiers: usize, distance: f32, current: usize) -> usize {
    let mut tier = 0;
    while tier + 1 < tiers {
        let boundary = switch_distance(switch, tier);
        let boundary = if tier < current {
            boundary * (1.0 - LOD_HYSTERESIS)
        } else {
            boundary * (1.0 + LOD_HYSTERESIS)
        };
        if distance > boundary {
            tier += 1;
        } else {
            break;
        }
    }
    tier
}

/// How much of an object to draw at `distance` when it disappears at
/// `draw_distance`: 1 is fully drawn, 0 is gone, and the last
/// `fade_length` before the draw distance blends between the two.
pub fn distance_fade(distance: f32, draw_distance: f32, fade_length: f32) -> f32 {
    if distance >= draw_distance {
        return 0.0;
    }
    if fade_length <= 0.0 || !fade_length.is_finite() {
        return 1.0;
    }
    ((draw_distance - distance) / fade_length).clamp(0.0, 1.0)
}

/// A deduplicating, bounded queue of asset IDs waiting for streaming.
#[derive(Debug, Clone)]
pub struct StreamQueue<T> {
    queue: VecDeque<T>,
    queued: HashSet<T>,
    capacity: usize,
}

impl<T: Copy + Eq + std::hash::Hash> StreamQueue<T> {
    /// Creates a queue with a minimum capacity of one.
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity.max(1)),
            queued: HashSet::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
        }
    }
    /// Adds a request unless it is already queued or capacity is exhausted.
    pub fn push(&mut self, value: T) -> bool {
        if self.queued.contains(&value) || self.queue.len() >= self.capacity {
            return false;
        }
        self.queue.push_back(value);
        self.queued.insert(value);
        true
    }
    /// Removes the oldest request.
    pub fn pop(&mut self) -> Option<T> {
        let value = self.queue.pop_front()?;
        self.queued.remove(&value);
        Some(value)
    }
    /// Number of pending requests.
    pub fn len(&self) -> usize {
        self.queue.len()
    }
    /// Whether no requests are pending.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_follows_switch_distances() {
        let switch = [10.0, 30.0];
        assert_eq!(select_lod_tier(&switch, 3, 5.0, 0), 0);
        assert_eq!(select_lod_tier(&switch, 3, 20.0, 1), 1);
        assert_eq!(select_lod_tier(&switch, 3, 100.0, 2), 2);
        assert_eq!(select_lod_tier(&switch, 1, 100.0, 0), 0);
        assert_eq!(select_lod_tier(&switch, 0, 100.0, 0), 0);
    }

    #[test]
    fn tier_waits_past_the_boundary_before_switching() {
        let switch = [10.0];
        // Moving away: stays detailed until 10% past the line.
        assert_eq!(select_lod_tier(&switch, 2, 10.5, 0), 0);
        assert_eq!(select_lod_tier(&switch, 2, 11.5, 0), 1);
        // Coming back: stays simple until 10% inside it.
        assert_eq!(select_lod_tier(&switch, 2, 9.5, 1), 1);
        assert_eq!(select_lod_tier(&switch, 2, 8.5, 1), 0);
    }

    #[test]
    fn missing_switch_distances_double() {
        assert_eq!(switch_distance(&[], 0), FIRST_SWITCH_DISTANCE);
        assert_eq!(switch_distance(&[10.0], 1), 20.0);
        assert_eq!(switch_distance(&[10.0, f32::NAN], 2), 40.0);
        assert_eq!(select_lod_tier(&[10.0], 3, 50.0, 2), 2);
    }

    #[test]
    fn fade_blends_over_the_last_stretch() {
        assert_eq!(distance_fade(10.0, 100.0, 5.0), 1.0);
        assert_eq!(distance_fade(97.5, 100.0, 5.0), 0.5);
        assert_eq!(distance_fade(100.0, 100.0, 5.0), 0.0);
        assert_eq!(distance_fade(150.0, 100.0, 5.0), 0.0);
        assert_eq!(distance_fade(99.0, 100.0, 0.0), 1.0);
    }

    #[test]
    fn lod_selection_is_sorted_and_falls_back() {
        let selector = LodSelector::new(vec![
            LodLevel {
                max_distance: 100.0,
            },
            LodLevel { max_distance: 10.0 },
        ]);
        assert_eq!(selector.select(5.0), Some(0));
        assert_eq!(selector.select(50.0), Some(1));
        assert_eq!(selector.select(f32::INFINITY), Some(1));
    }

    #[test]
    fn stream_queue_is_bounded_and_deduplicated() {
        let mut queue = StreamQueue::new(2);
        assert!(queue.push(1));
        assert!(!queue.push(1));
        assert!(queue.push(2));
        assert!(!queue.push(3));
        assert_eq!(queue.pop(), Some(1));
        assert!(queue.push(3));
    }

    #[test]
    fn lod_set_selects_concrete_tiers_and_rejects_mismatched_inputs() {
        let set = LodSet::new(vec!["high", "low"], vec![20.0, 100.0]).unwrap();
        assert_eq!(set.select(5.0), Some(&"high"));
        assert_eq!(set.select(200.0), Some(&"low"));
        assert!(LodSet::<u32>::new(vec![1], vec![1.0, 2.0]).is_none());
    }
}
