//! Collision enter/exit events between authored scene objects.

use std::collections::{BTreeSet, HashMap};

use engine_ecs::components::Collider;
use engine_ecs::prelude::World;
use engine_physics::PhysicsWorld;
use engine_scene::SceneObjectId;

/// Whether two objects started or stopped touching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactKind {
    /// The pair gained an active contact this step.
    Enter,
    /// The pair lost its last active contact this step.
    Exit,
}

impl ContactKind {
    /// The lowercase name scripts receive: `"enter"` or `"exit"`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Exit => "exit",
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

/// Remembers which pairs touch, so each step reports only changes.
#[derive(Debug, Default)]
pub struct ContactTracker {
    active: BTreeSet<(String, String)>,
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
        let current: BTreeSet<_> = physics
            .rapier
            .contact_pairs()
            .filter(|pair| pair.has_any_active_contact())
            .filter_map(|pair| {
                let first = object_ids.get(&pair.collider1)?;
                let second = object_ids.get(&pair.collider2)?;
                Some(if first <= second {
                    (first.clone(), second.clone())
                } else {
                    (second.clone(), first.clone())
                })
            })
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
            current
                .difference(&self.active)
                .map(|pair| event(ContactKind::Enter, pair)),
        );
        self.active = current;
        events
    }

    /// Forgets all contacts, as when the scene is replaced.
    pub fn clear(&mut self) {
        self.active.clear();
    }
}
