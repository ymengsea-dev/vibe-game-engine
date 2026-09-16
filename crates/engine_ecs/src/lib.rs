//! # engine_ecs
//!
//! Entity Component System core: `bevy_ecs` integration, systems, and queries.
//!
//! ## Status
//!
//! Milestone 3/4 progress, Milestone 7 in progress. [`Ecs`] owns a
//! `bevy_ecs` [`World`] with two named frame-stage schedules
//! ([`stages::Update`], [`stages::RenderExtract`] — see [`stages`] for
//! why these two), core components ([`components::Transform`],
//! [`components::GlobalTransform`], [`components::Camera`],
//! [`components::MeshRenderer`], [`components::RigidBody`],
//! [`components::Collider`], [`components::Name`]), entity hierarchy (`bevy_ecs`'s built-in
//! `ChildOf`/`Children`, with world-space transform propagation via
//! [`propagate::propagate_transforms`]), and resource passthroughs.
//! [`physics::sync_rigid_bodies`] copies each frame's simulated
//! [`components::RigidBody`] poses (from an `engine_physics::PhysicsWorld`
//! the caller steps separately) into [`components::Transform`], the same
//! "plain function called explicitly by the app loop" shape
//! [`render::extract_and_render`] uses for rendering.
//! [`components::Sprite`]/[`render::extract_sprites`] are the
//! 2D counterpart: every `(GlobalTransform, Sprite)` entity batched into
//! one instanced draw call per frame, not one `MeshRenderer`-style GPU
//! binding per entity. [`components::MeshRenderer`] itself holds asset
//! handles, not GPU resources directly — [`render::extract_and_render`]
//! resolves them against a caller-owned `engine_renderer::RenderAssets`
//! each frame, culls the meshes outside the caller-supplied
//! `engine_renderer::Frustum` from the scene draw list (returning
//! [`render::RenderStats`] with the total/drawn counts), and
//! [`render::despawn_mesh_renderer`] is the paired cleanup
//! (release the handles, then despawn), the same "caller's explicit job"
//! shape [`physics::sync_rigid_bodies`] already has for `RigidBody`/
//! `Collider`. `bevy_ecs` types
//! ([`World`], [`Schedule`], `Component`, `Resource`, `ChildOf`,
//! `Children`, ...) are re-exported wholesale via [`prelude`] rather than
//! wrapped, since ECS internals (queries, systems, bundles) are meant to
//! be used directly by game/engine code, not hidden behind an
//! engine-specific facade.

use bevy_ecs::change_detection::Mut;
use bevy_ecs::component::Mutable;
use bevy_ecs::resource::Resource;
use bevy_ecs::schedule::{IntoScheduleConfigs, Schedule, ScheduleLabel};
use bevy_ecs::system::ScheduleSystem;
use bevy_ecs::world::World;

pub mod animation;
pub mod audio;
pub mod components;
mod error;
pub mod particles;
pub mod physics;
pub mod propagate;
pub mod render;
pub mod stages;

pub use animation::{AnimationEvent, AnimationEvents, AnimationPlayer, update_animations};
pub use audio::{AudioEmitter, AudioListener, update_audio};
pub use error::EcsError;
pub use particles::{
    BlendMode, ParticleEmitter, ParticleEmitterConfig, Rng, extract_particles, update_particles,
};
pub use physics::sync_rigid_bodies;
pub use propagate::propagate_transforms;
pub use render::{
    RenderStats, SpriteTarget, despawn_instanced_mesh_renderer, despawn_mesh_renderer,
    despawn_skinned_mesh_renderer, despawn_vegetation_renderer, extract_and_render,
    extract_and_render_to, extract_sprites,
};

/// Re-exports the `bevy_ecs` prelude, plus [`Ecs`], the core components,
/// and the frame stage labels.
pub mod prelude {
    pub use bevy_ecs::prelude::*;

    pub use crate::Ecs;
    pub use crate::components::{
        AssetSource, Camera, Collider, Disabled, GlobalTransform, InstancedMeshRenderer, Lock,
        MeshRenderer, Name, RigidBody, Script, SkinnedMeshRenderer, Sprite, Static, Transform,
        VegetationRenderer,
    };
    pub use crate::despawn_instanced_mesh_renderer;
    pub use crate::despawn_mesh_renderer;
    pub use crate::despawn_skinned_mesh_renderer;
    pub use crate::despawn_vegetation_renderer;
    pub use crate::extract_sprites;
    pub use crate::particles::ParticleEmitter;
    pub use crate::propagate_transforms;
    pub use crate::stages::{RenderExtract, Update};
    pub use crate::sync_rigid_bodies;
}

/// Owns the ECS `World` (entities, components, resources) and its named
/// frame-stage schedules ([`stages::Update`], [`stages::RenderExtract`]).
///
/// Both stages are always registered (even with zero systems), so
/// [`Ecs::run_stage`] on either never fails with
/// [`EcsError::UnknownStage`] — that variant exists for custom labels
/// callers register themselves.
pub struct Ecs {
    world: World,
}

impl Ecs {
    /// An empty world (no user-spawned entities), with empty
    /// [`stages::Update`] and [`stages::RenderExtract`] schedules already
    /// registered.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_ecs::Ecs;
    ///
    /// let mut ecs = Ecs::new();
    /// let before = ecs.world().entities().count_spawned();
    /// ecs.world_mut().spawn(());
    /// assert_eq!(ecs.world().entities().count_spawned(), before + 1);
    /// ```
    pub fn new() -> Self {
        let mut world = World::new();
        world.add_schedule(Schedule::new(stages::Update));
        world.add_schedule(Schedule::new(stages::RenderExtract));
        Self { world }
    }

    /// Shared access to the world (for reading entities/components/resources
    /// without going through a system).
    pub fn world(&self) -> &World {
        &self.world
    }

    /// Mutable access to the world (for spawning/despawning entities,
    /// registering additional schedules, etc.).
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Adds `systems` to the schedule labeled `stage`. Builder-style:
    /// chain multiple calls, mixing stages freely.
    ///
    /// If `stage` has no registered schedule yet (true for any label other
    /// than [`stages::Update`]/[`stages::RenderExtract`] until first use),
    /// one is created automatically.
    ///
    /// # Example
    ///
    /// ```
    /// use engine_ecs::Ecs;
    /// use engine_ecs::stages::Update;
    ///
    /// fn hello() {}
    ///
    /// let mut ecs = Ecs::new();
    /// ecs.add_systems(Update, hello);
    /// ecs.run_stage(Update).unwrap();
    /// ```
    pub fn add_systems<M>(
        &mut self,
        stage: impl ScheduleLabel,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.world
            .resource_mut::<bevy_ecs::schedule::Schedules>()
            .add_systems(stage, systems);
        self
    }

    /// Runs the schedule labeled `stage` once, against the world.
    ///
    /// # Errors
    ///
    /// Returns [`EcsError::UnknownStage`] if `stage` has no registered
    /// schedule (never happens for [`stages::Update`]/
    /// [`stages::RenderExtract`] — see [`Ecs::new`]).
    pub fn run_stage(&mut self, stage: impl ScheduleLabel) -> Result<(), EcsError> {
        self.world
            .try_run_schedule(stage)
            .map_err(|err| EcsError::UnknownStage(err.to_string()))
    }

    /// Runs the [`stages::Update`] schedule once. Shorthand for
    /// `run_stage(Update)`, which cannot fail (see [`Ecs::run_stage`]).
    pub fn run_update(&mut self) {
        self.world.run_schedule(stages::Update);
    }

    /// Runs the [`stages::RenderExtract`] schedule once. Shorthand for
    /// `run_stage(RenderExtract)`, which cannot fail (see
    /// [`Ecs::run_stage`]).
    pub fn run_render_extract(&mut self) {
        self.world.run_schedule(stages::RenderExtract);
    }

    /// Inserts (or overwrites) a resource. Builder-style: chain multiple
    /// calls.
    pub fn insert_resource<R: Resource>(&mut self, resource: R) -> &mut Self {
        self.world.insert_resource(resource);
        self
    }

    /// Shared access to a resource, or `None` if `R` hasn't been inserted.
    ///
    /// Deliberately fallible rather than mirroring `World::resource`'s
    /// panic-on-missing contract — production code here returns `Result`/
    /// `Option`, not panics.
    pub fn resource<R: Resource>(&self) -> Option<&R> {
        self.world.get_resource::<R>()
    }

    /// Mutable access to a resource, or `None` if `R` hasn't been
    /// inserted. See [`Ecs::resource`] for why this is fallible.
    pub fn resource_mut<R: Resource<Mutability = Mutable>>(&mut self) -> Option<&mut R> {
        self.world.get_resource_mut::<R>().map(Mut::into_inner)
    }
}

impl Default for Ecs {
    /// [`Ecs::new`].
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::prelude::{Component, Resource};

    #[derive(Component)]
    struct Marker;

    #[derive(Resource, Default, Debug, PartialEq)]
    struct Counter(u32);

    fn increment_counter(mut counter: bevy_ecs::system::ResMut<Counter>) {
        counter.0 += 1;
    }

    // `bevy_ecs` 0.19 reserves a handful of entity indices internally in a
    // fresh `World` (built-in observers/lifecycle hooks), so these tests
    // assert against that baseline rather than an absolute zero — the
    // exact count is a `bevy_ecs` implementation detail, not part of our
    // contract. Also: `Entities::len()` counts allocated indices, not
    // currently-spawned entities (freed indices are reused, not shrunk),
    // so `count_spawned()` — documented upstream as "intended only to be
    // used as a diagnostic for tests" — is the correct query here.

    #[test]
    fn new_produces_a_consistent_baseline_entity_count() {
        let baseline = Ecs::new().world().entities().count_spawned();
        let ecs = Ecs::new();
        assert_eq!(ecs.world().entities().count_spawned(), baseline);
    }

    #[test]
    fn world_mut_can_spawn_entities() {
        let mut ecs = Ecs::new();
        let baseline = ecs.world().entities().count_spawned();
        ecs.world_mut().spawn(Marker);
        ecs.world_mut().spawn(Marker);
        assert_eq!(ecs.world().entities().count_spawned(), baseline + 2);
    }

    #[test]
    fn run_update_with_no_systems_is_a_harmless_no_op() {
        let mut ecs = Ecs::new();
        ecs.run_update();
        ecs.run_update();
        // No panic, nothing to assert beyond "didn't crash".
    }

    #[test]
    fn added_system_runs_once_per_run_stage_call() {
        let mut ecs = Ecs::new();
        ecs.world_mut().insert_resource(Counter::default());
        ecs.add_systems(stages::Update, increment_counter);

        ecs.run_stage(stages::Update).unwrap();
        assert_eq!(ecs.world().resource::<Counter>().0, 1);

        ecs.run_stage(stages::Update).unwrap();
        assert_eq!(ecs.world().resource::<Counter>().0, 2);
    }

    #[test]
    fn default_matches_new() {
        let baseline = Ecs::new().world().entities().count_spawned();
        let ecs = Ecs::default();
        assert_eq!(ecs.world().entities().count_spawned(), baseline);
    }

    #[test]
    fn update_and_render_extract_stages_run_independently() {
        let mut ecs = Ecs::new();
        ecs.insert_resource(Counter::default());
        ecs.add_systems(stages::Update, increment_counter);

        ecs.run_stage(stages::RenderExtract).unwrap();
        assert_eq!(*ecs.resource::<Counter>().unwrap(), Counter(0));

        ecs.run_stage(stages::Update).unwrap();
        assert_eq!(*ecs.resource::<Counter>().unwrap(), Counter(1));
    }

    #[test]
    fn run_stage_on_unregistered_custom_label_errors() {
        #[derive(bevy_ecs::schedule::ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
        struct CustomStage;

        let mut ecs = Ecs::new();
        let err = ecs.run_stage(CustomStage).unwrap_err();
        assert!(matches!(err, EcsError::UnknownStage(_)));
    }

    #[test]
    fn add_systems_to_unregistered_custom_label_creates_it() {
        #[derive(bevy_ecs::schedule::ScheduleLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
        struct CustomStage;

        let mut ecs = Ecs::new();
        ecs.insert_resource(Counter::default());
        ecs.add_systems(CustomStage, increment_counter);

        ecs.run_stage(CustomStage).unwrap();
        assert_eq!(*ecs.resource::<Counter>().unwrap(), Counter(1));
    }

    #[test]
    fn resource_helpers_round_trip() {
        let mut ecs = Ecs::new();
        assert!(ecs.resource::<Counter>().is_none());

        ecs.insert_resource(Counter(5));
        assert_eq!(*ecs.resource::<Counter>().unwrap(), Counter(5));

        ecs.resource_mut::<Counter>().unwrap().0 += 1;
        assert_eq!(*ecs.resource::<Counter>().unwrap(), Counter(6));
    }
}
