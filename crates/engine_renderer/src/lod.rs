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
