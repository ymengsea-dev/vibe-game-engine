//! The coarse game state (`menu`, `playing`, `paused`, `game_over`) and how it
//! pauses the simulation.

use engine_ecs::prelude::World;
use engine_scene::{GameState, GameStateName};
use engine_scripting::GameCommand;

/// The current state, or the default when the world has none yet.
pub(crate) fn current(world: &World) -> GameStateName {
    world
        .get_resource::<GameState>()
        .map(|state| state.current)
        .unwrap_or_default()
}

/// Whether the current state freezes the simulation.
pub(crate) fn simulation_paused(world: &World) -> bool {
    current(world) == GameStateName::Paused
}

/// Creates the state resource from the scene's `start_state` (default
/// `playing`) only when none exists, so the state survives scene changes.
/// Returns the state now in force.
pub(crate) fn init_state(world: &mut World, start: Option<GameStateName>) -> GameStateName {
    if world.get_resource::<GameState>().is_none() {
        world.insert_resource(GameState {
            current: start.unwrap_or_default(),
        });
    }
    current(world)
}

/// Applies the `SetGameState` commands in order. Unknown names and repeats of
/// the current state are skipped. Returns the net change as `(first old, last
/// new)`, or `None` when the batch ends where it began.
pub(crate) fn apply_state_commands(
    world: &mut World,
    commands: &[GameCommand],
) -> Option<(GameStateName, GameStateName)> {
    let mut change: Option<(GameStateName, GameStateName)> = None;
    for command in commands {
        let GameCommand::SetGameState { state } = command else {
            continue;
        };
        let Some(new) = GameStateName::from_name(state) else {
            tracing::warn!(%state, "unknown game state ignored");
            continue;
        };
        let old = current(world);
        if old == new {
            continue;
        }
        world.insert_resource(GameState { current: new });
        change = merge_change(change, Some((old, new)));
    }
    change
}

/// Joins a pending change with a later one: the first `old` and the last
/// `new` win, and a change that ends where it began is no change.
pub(crate) fn merge_change(
    pending: Option<(GameStateName, GameStateName)>,
    later: Option<(GameStateName, GameStateName)>,
) -> Option<(GameStateName, GameStateName)> {
    let merged = match (pending, later) {
        (Some((old, _)), Some((_, new))) => (old, new),
        (one, None) | (None, one) => return one,
    };
    (merged.0 != merged.1).then_some(merged)
}

/// The step the camera presents with: a paused simulation freezes follow,
/// orbit and shake.
pub(crate) fn camera_dt(paused: bool, dt: f32) -> f32 {
    if paused { 0.0 } else { dt }
}
