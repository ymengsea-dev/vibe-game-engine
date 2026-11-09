//! Destroying, showing/hiding and enabling/disabling scene objects.
//!
//! Commands apply to the object and every descendant (the `ChildOf` chain).

use std::collections::{HashMap, HashSet};

use engine_ecs::components::{Collider, Disabled, Hidden, RigidBody};
use engine_ecs::prelude::{Children, Entity, World};
use engine_physics::PhysicsWorld;
use engine_scene::SceneObjectId;
use engine_scripting::GameCommand;
use engine_ui::{NodeId, Ui};

use super::collisions::ContactTracker;
use super::commands::find_object;

/// The world, physics and UI the object commands change.
pub(super) struct ObjectTargets<'a> {
    pub world: &'a mut World,
    pub physics: &'a mut PhysicsWorld,
    pub ui: &'a mut Ui,
}

/// Runtime bookkeeping that must forget destroyed objects.
pub(super) struct ObjectState<'a> {
    pub ui_nodes: &'a mut HashMap<NodeId, String>,
    pub contacts: &'a mut ContactTracker,
    pub warned: &'a mut HashSet<(String, &'static str)>,
    pub grounded: &'a mut HashMap<String, bool>,
    pub scene_order: &'a mut Vec<Entity>,
    pub active_camera: &'a mut Option<Entity>,
    /// Object IDs removed by `Destroy`, descendants included, for hosts that
    /// must forget them (such as their scripts).
    pub destroyed: &'a mut Vec<String>,
}

fn warn_once(warned: &mut HashSet<(String, &'static str)>, id: &str, kind: &'static str) {
    if warned.insert((id.to_owned(), kind)) {
        tracing::warn!(object = %id, kind, "object command skipped: no such object");
    }
}

/// The entity and every descendant, parents before children.
pub(super) fn subtree(world: &World, root: Entity) -> Vec<Entity> {
    let mut out = vec![root];
    let mut next = 0;
    while let Some(&entity) = out.get(next) {
        next += 1;
        if let Some(children) = world.get::<Children>(entity) {
            for child in children.iter() {
                if !out.contains(child) {
                    out.push(*child);
                }
            }
        }
    }
    out
}

/// Applies `Destroy`, `SetVisible` and `SetEnabled` in command order; other
/// commands are ignored. A missing object skips the command and warns once per
/// `(object, kind)`.
///
/// `release` is called once per removed entity, before it is despawned, to
/// give back its GPU assets.
pub(super) fn apply_object_commands(
    targets: ObjectTargets<'_>,
    state: ObjectState<'_>,
    release: &mut dyn FnMut(&World, Entity),
    commands: &[GameCommand],
) {
    let ObjectTargets { world, physics, ui } = targets;
    let ObjectState {
        ui_nodes,
        contacts,
        warned,
        grounded,
        scene_order,
        active_camera,
        destroyed,
    } = state;
    for command in commands {
        let (object_id, kind) = match command {
            GameCommand::Destroy { object_id } => (object_id, "destroy"),
            GameCommand::SetVisible { object_id, .. } => (object_id, "set_visible"),
            GameCommand::SetEnabled { object_id, .. } => (object_id, "set_enabled"),
            _ => continue,
        };
        let entity = find_object(world, object_id);
        let node_of = |ui_nodes: &HashMap<NodeId, String>, id: &str| {
            ui_nodes
                .iter()
                .find_map(|(node, owner)| (owner == id).then_some(*node))
        };
        if entity.is_none() && node_of(ui_nodes, object_id).is_none() {
            warn_once(warned, object_id, kind);
            continue;
        }
        let members = entity.map_or_else(Vec::new, |entity| subtree(world, entity));
        let ids: Vec<String> = members
            .iter()
            .filter_map(|member| world.get::<SceneObjectId>(*member).map(|id| id.0.clone()))
            .collect();
        match command {
            GameCommand::SetVisible { visible, .. } => {
                for member in &members {
                    set_marker::<Hidden>(world, *member, !*visible);
                }
                for id in ids.iter().chain(entity.is_none().then_some(object_id)) {
                    if let Some(node) = node_of(ui_nodes, id) {
                        ui.set_visible(node, *visible);
                    }
                }
            }
            GameCommand::SetEnabled { enabled, .. } => {
                for member in &members {
                    set_marker::<Disabled>(world, *member, !*enabled);
                    set_physics_enabled(world, physics, *member, *enabled);
                }
                for id in ids.iter().chain(entity.is_none().then_some(object_id)) {
                    if let Some(node) = node_of(ui_nodes, id) {
                        ui.set_visible(node, *enabled);
                    }
                }
            }
            GameCommand::Destroy { .. } => {
                let mut removed: HashSet<String> = ids.iter().cloned().collect();
                removed.insert(object_id.clone());
                for member in &members {
                    release(world, *member);
                    remove_physics(world, physics, *member);
                }
                for member in &members {
                    world.despawn(*member);
                }
                // The UI arena has no removal: hide the node and unmap it.
                ui_nodes.retain(|node, owner| {
                    let gone = removed.contains(owner);
                    if gone {
                        ui.set_visible(*node, false);
                    }
                    !gone
                });
                destroyed.extend(removed.iter().cloned());
                contacts.forget(&removed);
                grounded.retain(|id, _| !removed.contains(id));
                scene_order.retain(|entity| !members.contains(entity));
                if active_camera.is_some_and(|camera| members.contains(&camera)) {
                    *active_camera = None;
                }
            }
            _ => {}
        }
    }
}

/// Carries an authored `Disabled` down to the descendants of each root that
/// has it, as `SetEnabled { enabled: false }` does: the children get the marker
/// and their bodies and colliders switch off. Call once colliders exist.
pub(super) fn disable_authored_subtrees(
    world: &mut World,
    physics: &mut PhysicsWorld,
    roots: &[Entity],
) {
    for root in roots {
        if world.get::<Disabled>(*root).is_none() {
            continue;
        }
        for member in subtree(world, *root) {
            set_marker::<Disabled>(world, member, true);
            set_physics_enabled(world, physics, member, false);
        }
    }
}

/// Inserts (`on`) or removes the marker component.
fn set_marker<M: engine_ecs::prelude::Component + Default>(
    world: &mut World,
    entity: Entity,
    on: bool,
) {
    let mut entity = world.entity_mut(entity);
    if on {
        entity.insert(M::default());
    } else {
        entity.remove::<M>();
    }
}

pub(crate) fn set_physics_enabled(
    world: &World,
    physics: &mut PhysicsWorld,
    entity: Entity,
    enabled: bool,
) {
    let rapier = &mut physics.rapier;
    if let Some(body) = world.get::<RigidBody>(entity)
        && let Some(body) = rapier.bodies.get_mut(body.0)
    {
        body.set_enabled(enabled);
        if enabled {
            body.wake_up(true);
        }
    }
    if let Some(collider) = world.get::<Collider>(entity)
        && let Some(collider) = rapier.colliders.get_mut(collider.0)
    {
        collider.set_enabled(enabled);
    }
}

/// Removes the entity's body (with its colliders) or its standalone collider.
pub(super) fn remove_physics(world: &World, physics: &mut PhysicsWorld, entity: Entity) {
    if let Some(body) = world.get::<RigidBody>(entity) {
        physics.rapier.remove_body(body.0);
    } else if let Some(collider) = world.get::<Collider>(entity) {
        let rapier = &mut physics.rapier;
        rapier
            .colliders
            .remove(collider.0, &mut rapier.islands, &mut rapier.bodies, true);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use engine_ecs::components::{
        Camera as CameraComponent, Collider, Disabled, Hidden, RigidBody, Transform,
    };
    use engine_ecs::prelude::{ChildOf, Entity, World};
    use engine_physics::{
        ColliderBuilder, PhysicsWorld, QueryFilter, RigidBodyBuilder, cast_ray_filtered,
    };
    use engine_scene::SceneObjectId;
    use engine_scripting::GameCommand;
    use engine_ui::{Color, NodeId, Style, Ui, Widget};
    use glam::Vec3;

    use super::*;
    use crate::gameplay::collisions::{ContactKind, ContactTracker};
    use crate::gameplay::find_object;

    struct Fixture {
        world: World,
        physics: PhysicsWorld,
        ui: Ui,
        ui_nodes: HashMap<NodeId, String>,
        contacts: ContactTracker,
        warned: HashSet<(String, &'static str)>,
        grounded: HashMap<String, bool>,
        scene_order: Vec<Entity>,
        active_camera: Option<Entity>,
        destroyed: Vec<String>,
        released: Vec<Entity>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                world: World::new(),
                physics: PhysicsWorld::default(),
                ui: Ui::new(800.0, 600.0),
                ui_nodes: HashMap::new(),
                contacts: ContactTracker::default(),
                warned: HashSet::new(),
                grounded: HashMap::new(),
                scene_order: Vec::new(),
                active_camera: None,
                destroyed: Vec::new(),
                released: Vec::new(),
            }
        }

        fn run(&mut self, commands: &[GameCommand]) {
            let released = &mut self.released;
            apply_object_commands(
                ObjectTargets {
                    world: &mut self.world,
                    physics: &mut self.physics,
                    ui: &mut self.ui,
                },
                ObjectState {
                    ui_nodes: &mut self.ui_nodes,
                    contacts: &mut self.contacts,
                    warned: &mut self.warned,
                    grounded: &mut self.grounded,
                    scene_order: &mut self.scene_order,
                    active_camera: &mut self.active_camera,
                    destroyed: &mut self.destroyed,
                },
                &mut |_, entity| released.push(entity),
                commands,
            );
        }

        fn object(&mut self, id: &str, parent: Option<&str>) -> Entity {
            let mut entity = self
                .world
                .spawn((SceneObjectId(id.into()), Transform::default()));
            if let Some(parent) = parent {
                let parent = find_object(entity.world(), parent).expect("parent");
                entity.insert(ChildOf(parent));
            }
            let entity = entity.id();
            self.scene_order.push(entity);
            entity
        }

        fn body(&mut self, id: &str, builder: RigidBodyBuilder, collider: ColliderBuilder) {
            let (body, collider) = RigidBody::spawn(&mut self.physics, builder, collider);
            let entity = self.object(id, None);
            self.world.entity_mut(entity).insert((body, collider));
        }
    }

    fn visible(id: &str, visible: bool) -> GameCommand {
        GameCommand::SetVisible {
            object_id: id.into(),
            visible,
        }
    }
    fn enabled(id: &str, enabled: bool) -> GameCommand {
        GameCommand::SetEnabled {
            object_id: id.into(),
            enabled,
        }
    }
    fn destroy(id: &str) -> GameCommand {
        GameCommand::Destroy {
            object_id: id.into(),
        }
    }

    #[test]
    fn set_visible_hides_the_whole_subtree_and_shows_it_again() {
        let mut f = Fixture::new();
        let parent = f.object("parent", None);
        let child = f.object("child", Some("parent"));
        let grandchild = f.object("grandchild", Some("child"));
        let other = f.object("other", None);

        f.run(&[visible("parent", false)]);
        for entity in [parent, child, grandchild] {
            assert!(f.world.get::<Hidden>(entity).is_some());
        }
        assert!(f.world.get::<Hidden>(other).is_none());

        // A child hidden on its own is shown again with its parent.
        f.run(&[visible("parent", true)]);
        for entity in [parent, child, grandchild] {
            assert!(f.world.get::<Hidden>(entity).is_none());
        }
        assert!(f.warned.is_empty());
    }

    #[test]
    fn set_enabled_marks_the_subtree_disabled() {
        let mut f = Fixture::new();
        let parent = f.object("parent", None);
        let child = f.object("child", Some("parent"));
        f.run(&[enabled("parent", false)]);
        assert!(f.world.get::<Disabled>(parent).is_some());
        assert!(f.world.get::<Disabled>(child).is_some());
        f.run(&[enabled("parent", true)]);
        assert!(f.world.get::<Disabled>(parent).is_none());
        assert!(f.world.get::<Disabled>(child).is_none());
    }

    #[test]
    fn authored_ui_follows_visibility_and_enabling() {
        let mut f = Fixture::new();
        f.object("label", None);
        let node = f.ui.add_screen(
            Style::default(),
            Widget::Label {
                text: "hi".into(),
                color: Color::WHITE,
            },
        );
        f.ui_nodes.insert(node, "label".into());
        f.run(&[visible("label", false)]);
        assert!(!f.ui.is_visible(node));
        f.run(&[visible("label", true)]);
        assert!(f.ui.is_visible(node));
        f.run(&[enabled("label", false)]);
        assert!(!f.ui.is_visible(node));
        f.run(&[enabled("label", true)]);
        assert!(f.ui.is_visible(node));
    }

    fn step(f: &mut Fixture, n: usize) -> Vec<crate::gameplay::CollisionEvent> {
        let mut events = Vec::new();
        for _ in 0..n {
            f.physics.step(1.0 / 60.0).expect("step");
            events.extend(f.contacts.sample(&f.world, &f.physics));
        }
        events
    }

    fn box_on_floor(f: &mut Fixture) {
        f.body(
            "floor",
            RigidBodyBuilder::fixed().translation(Vec3::new(0.0, -0.5, 0.0)),
            ColliderBuilder::cuboid(20.0, 0.5, 20.0),
        );
        f.body(
            "box",
            RigidBodyBuilder::dynamic().translation(Vec3::new(0.0, 0.5, 0.0)),
            ColliderBuilder::cuboid(0.5, 0.5, 0.5),
        );
    }

    fn box_y(f: &Fixture) -> f32 {
        let entity = find_object(&f.world, "box").expect("box");
        let body = f.world.get::<RigidBody>(entity).expect("body").0;
        f.physics
            .rapier
            .bodies
            .get(body)
            .expect("body")
            .translation()
            .y
    }

    #[test]
    fn set_enabled_false_disables_the_body_and_stops_contacts() {
        let mut f = Fixture::new();
        box_on_floor(&mut f);
        let events = step(&mut f, 10);
        assert!(events.iter().any(|e| e.kind == ContactKind::Enter));

        f.run(&[enabled("box", false)]);
        let events = step(&mut f, 30);
        assert!(
            events.iter().any(|e| e.kind == ContactKind::Exit),
            "the contact ends: {events:?}"
        );
        let resting = box_y(&f);
        // Gravity would have moved a live body; a disabled one holds still.
        step(&mut f, 30);
        assert_eq!(box_y(&f), resting);
        // No raycast hit on the disabled collider.
        let hit = cast_ray_filtered(
            &f.physics,
            Vec3::new(0.0, 5.0, 0.0),
            Vec3::NEG_Y,
            10.0,
            QueryFilter::default(),
        )
        .expect("ray");
        let floor = find_object(&f.world, "floor").expect("floor");
        let floor_collider = f.world.get::<Collider>(floor).expect("collider").0;
        assert_eq!(hit.map(|hit| hit.collider), Some(floor_collider));

        // Re-enabled, it takes part again: lift it and it falls.
        f.run(&[enabled("box", true)]);
        let entity = find_object(&f.world, "box").expect("box");
        let body = f.world.get::<RigidBody>(entity).expect("body").0;
        if let Some(body) = f.physics.rapier.bodies.get_mut(body) {
            body.set_translation(Vec3::new(0.0, 3.0, 0.0), true);
        }
        step(&mut f, 20);
        assert!(box_y(&f) < 3.0 - 0.1, "box fell, y = {}", box_y(&f));
    }

    #[test]
    fn a_disabled_authored_box_does_not_fall_and_has_no_contacts() {
        // `spawn_scene_colliders` calls `set_physics_enabled(.., false)` for an
        // entity that carries `Disabled`; do the same here.
        let mut f = Fixture::new();
        box_on_floor(&mut f);
        let entity = find_object(&f.world, "box").expect("box");
        f.world.entity_mut(entity).insert(Disabled);
        set_physics_enabled(&f.world, &mut f.physics, entity, false);
        if let Some(body) = f.world.get::<RigidBody>(entity)
            && let Some(body) = f.physics.rapier.bodies.get_mut(body.0)
        {
            body.set_translation(Vec3::new(0.0, 4.0, 0.0), true);
        }
        let events = step(&mut f, 60);
        assert!(events.is_empty(), "no contacts: {events:?}");
        assert_eq!(box_y(&f), 4.0, "it did not fall");
    }

    #[test]
    fn destroy_removes_subtree_body_and_assets() {
        let mut f = Fixture::new();
        let parent = f.object("parent", None);
        let child = f.object("child", Some("parent"));
        let kept = f.object("kept", None);
        let (body, collider) = RigidBody::spawn(
            &mut f.physics,
            RigidBodyBuilder::dynamic(),
            ColliderBuilder::ball(0.5),
        );
        f.world.entity_mut(child).insert((body, collider));
        let (body2, collider2) = RigidBody::spawn(
            &mut f.physics,
            RigidBodyBuilder::fixed(),
            ColliderBuilder::ball(0.5),
        );
        f.world.entity_mut(kept).insert((body2, collider2));
        let bodies_before = f.physics.rapier.bodies.len();
        let node = f.ui.add_screen(
            Style::default(),
            Widget::Panel {
                color: Color::WHITE,
            },
        );
        f.ui_nodes.insert(node, "parent".into());
        f.contacts.insert_for_test("child", "kept");
        f.contacts.insert_for_test("kept", "other");

        f.run(&[destroy("parent")]);

        assert!(find_object(&f.world, "parent").is_none());
        assert!(find_object(&f.world, "child").is_none());
        assert!(f.world.get_entity(parent).is_err());
        assert!(f.world.get_entity(child).is_err());
        assert!(find_object(&f.world, "kept").is_some());
        assert_eq!(f.physics.rapier.bodies.len(), bodies_before - 1);
        let mut released = f.released.clone();
        released.sort();
        let mut expected = vec![parent, child];
        expected.sort();
        assert_eq!(
            released, expected,
            "assets released once per removed entity"
        );
        assert!(!f.ui.is_visible(node));
        assert!(f.ui_nodes.is_empty());
        assert_eq!(
            f.contacts.pairs_for_test(),
            vec![("kept".into(), "other".into())]
        );
        assert!(!f.scene_order.contains(&parent) && !f.scene_order.contains(&child));
        assert!(f.scene_order.contains(&kept));
        let mut destroyed = f.destroyed.clone();
        destroyed.sort();
        assert_eq!(destroyed, ["child", "parent"], "descendants are reported");
    }

    #[test]
    fn an_authored_disabled_parent_disables_its_children_and_bodies() {
        let mut f = Fixture::new();
        let parent = f.object("parent", None);
        let child = f.object("child", Some("parent"));
        let sibling = f.object("sibling", None);
        let (body, collider) = RigidBody::spawn(
            &mut f.physics,
            RigidBodyBuilder::dynamic(),
            ColliderBuilder::ball(0.5),
        );
        f.world.entity_mut(child).insert((body, collider));
        f.world.entity_mut(parent).insert(Disabled);
        disable_authored_subtrees(&mut f.world, &mut f.physics, &[parent, child, sibling]);
        assert!(f.world.get::<Disabled>(child).is_some());
        assert!(f.world.get::<Disabled>(sibling).is_none());
        let body = f.world.get::<RigidBody>(child).expect("body").0;
        assert!(!f.physics.rapier.bodies[body].is_enabled());
    }

    #[test]
    fn a_ui_element_mounted_hidden_shows_after_enable() {
        let mut f = Fixture::new();
        f.object("label", None);
        let node = f.ui.add_screen(
            Style::default(),
            Widget::Label {
                text: "hi".into(),
                color: Color::WHITE,
            },
        );
        f.ui.set_visible(node, false);
        f.ui_nodes.insert(node, "label".into());
        f.run(&[enabled("label", true)]);
        assert!(f.ui.is_visible(node));
        f.ui.set_visible(node, false);
        f.run(&[visible("label", true)]);
        assert!(f.ui.is_visible(node));
    }

    #[test]
    fn destroying_the_active_camera_chooses_the_next() {
        let mut f = Fixture::new();
        let camera = engine_renderer::Camera::new(Vec3::ZERO, Vec3::NEG_Z, 1.0);
        let first = f.object("cam-a", None);
        let second = f.object("cam-b", None);
        f.world.entity_mut(first).insert(CameraComponent(camera));
        f.world.entity_mut(second).insert(CameraComponent(camera));
        f.active_camera = Some(first);

        f.run(&[destroy("cam-a")]);
        assert_eq!(f.active_camera, None);

        let mut current = f.active_camera;
        let mut warned = HashSet::new();
        let view = crate::gameplay::camera::present_in_world(
            &mut f.world,
            &mut current,
            &f.scene_order,
            1.0,
            &Default::default(),
            &mut warned,
        );
        assert!(view.is_some());
        assert_eq!(current, Some(second));
    }

    #[test]
    fn object_commands_on_missing_ids_warn_once() {
        let mut f = Fixture::new();
        f.object("real", None);
        let batch = [
            destroy("ghost"),
            visible("ghost", false),
            enabled("ghost", false),
        ];
        f.run(&batch);
        f.run(&batch);
        assert_eq!(f.warned.len(), 3);
        for kind in ["destroy", "set_visible", "set_enabled"] {
            assert!(f.warned.contains(&("ghost".to_owned(), kind)));
        }
        assert!(find_object(&f.world, "real").is_some());
    }
}
