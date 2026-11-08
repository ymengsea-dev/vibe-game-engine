//! Collision enter/exit events between authored scene objects.

use std::collections::{BTreeSet, HashMap};

use engine_ecs::components::Collider;
use engine_ecs::prelude::World;
use engine_physics::{ColliderHandle, PhysicsWorld};
use engine_scene::SceneObjectId;

/// Whether two objects started or stopped touching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactKind {
    /// The pair gained an active contact this step.
    Enter,
    /// The pair lost its last active contact this step.
    Exit,
    /// An object entered a trigger volume this step.
    TriggerEnter,
    /// An object left a trigger volume this step.
    TriggerExit,
}

impl ContactKind {
    /// The lowercase name scripts receive: `"enter"`, `"exit"`,
    /// `"trigger_enter"` or `"trigger_exit"`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Exit => "exit",
            Self::TriggerEnter => "trigger_enter",
            Self::TriggerExit => "trigger_exit",
        }
    }
}

/// One contact transition between two objects with scene IDs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionEvent {
    /// Enter or exit.
    pub kind: ContactKind,
    /// The lexically smaller object ID of the pair.
    pub first: String,
    /// The lexically larger object ID of the pair.
    pub second: String,
}

impl CollisionEvent {
    /// Whether `object_id` is one side of this contact.
    pub fn involves(&self, object_id: &str) -> bool {
        self.first == object_id || self.second == object_id
    }

    /// The other side of the contact, when `object_id` is one side.
    pub fn other(&self, object_id: &str) -> Option<&str> {
        if self.first == object_id {
            Some(&self.second)
        } else if self.second == object_id {
            Some(&self.first)
        } else {
            None
        }
    }
}

/// Remembers which pairs touch or overlap, so each step reports only changes.
#[derive(Debug, Default)]
pub struct ContactTracker {
    active: BTreeSet<(String, String)>,
    active_triggers: BTreeSet<(String, String)>,
}

impl ContactTracker {
    /// Compares this step's contacts with the last sample.
    /// Exits are reported before enters; pairs are in ID order.
    pub fn sample(&mut self, world: &World, physics: &PhysicsWorld) -> Vec<CollisionEvent> {
        let object_ids: HashMap<_, _> = world
            .iter_entities()
            .filter_map(|entity| {
                Some((
                    entity.get::<Collider>()?.0,
                    entity.get::<SceneObjectId>()?.0.clone(),
                ))
            })
            .collect();
        let ordered = |a: &ColliderHandle, b: &ColliderHandle| -> Option<(String, String)> {
            let first = object_ids.get(a)?;
            let second = object_ids.get(b)?;
            Some(if first <= second {
                (first.clone(), second.clone())
            } else {
                (second.clone(), first.clone())
            })
        };
        let current: BTreeSet<_> = physics
            .rapier
            .contact_pairs()
            .filter(|pair| pair.has_any_active_contact())
            .filter_map(|pair| ordered(&pair.collider1, &pair.collider2))
            .collect();
        let current_triggers: BTreeSet<_> = physics
            .rapier
            .intersection_pairs()
            .filter(|(_, _, _, _, intersecting)| *intersecting)
            .filter_map(|(a, _, b, _, _)| ordered(&a, &b))
            .collect();
        let event = |kind, (first, second): &(String, String)| CollisionEvent {
            kind,
            first: first.clone(),
            second: second.clone(),
        };
        let mut events: Vec<_> = self
            .active
            .difference(&current)
            .map(|pair| event(ContactKind::Exit, pair))
            .collect();
        events.extend(
            self.active_triggers
                .difference(&current_triggers)
                .map(|pair| event(ContactKind::TriggerExit, pair)),
        );
        events.extend(
            current
                .difference(&self.active)
                .map(|pair| event(ContactKind::Enter, pair)),
        );
        events.extend(
            current_triggers
                .difference(&self.active_triggers)
                .map(|pair| event(ContactKind::TriggerEnter, pair)),
        );
        self.active = current;
        self.active_triggers = current_triggers;
        events
    }

    /// Forgets every pair that names one of `ids`, without reporting an exit
    /// (the objects are gone).
    pub(crate) fn forget(&mut self, ids: &std::collections::HashSet<String>) {
        let names = |(a, b): &(String, String)| ids.contains(a) || ids.contains(b);
        self.active.retain(|pair| !names(pair));
        self.active_triggers.retain(|pair| !names(pair));
    }

    #[cfg(test)]
    pub(crate) fn insert_for_test(&mut self, a: &str, b: &str) {
        self.active.insert((a.to_owned(), b.to_owned()));
    }

    #[cfg(test)]
    pub(crate) fn pairs_for_test(&self) -> Vec<(String, String)> {
        self.active.iter().cloned().collect()
    }

    /// Forgets all contacts, as when the scene is replaced.
    pub fn clear(&mut self) {
        self.active.clear();
        self.active_triggers.clear();
    }
}
