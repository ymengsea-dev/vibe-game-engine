//! The coarse game state that rules, Lua and Rust games share.

use serde::{Deserialize, Serialize};

/// The fixed set of game states a rule, script or Rust game can switch between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum GameStateName {
    /// A title or menu screen.
    Menu,
    /// Normal play; the state a game starts in unless a scene says otherwise.
    #[default]
    Playing,
    /// Gameplay is frozen.
    Paused,
    /// The round is over.
    GameOver,
}

impl GameStateName {
    /// Every state, in editor display order.
    pub const ALL: [Self; 4] = [Self::Menu, Self::Playing, Self::Paused, Self::GameOver];

    /// The stable lowercase name scripts use.
    pub fn name(self) -> &'static str {
        match self {
            Self::Menu => "menu",
            Self::Playing => "playing",
            Self::Paused => "paused",
            Self::GameOver => "game_over",
        }
    }

    /// Parses a script-facing name; only the exact lowercase names match.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.name() == name)
    }

    /// Plain label for editor menus.
    pub fn label(self) -> &'static str {
        match self {
            Self::Menu => "Menu",
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::GameOver => "Game over",
        }
    }
}

/// The running game's current state; survives scene changes within one run.
#[derive(bevy_ecs::resource::Resource, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GameState {
    /// The state the game is in now.
    pub current: GameStateName,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_state_names_are_fixed() {
        assert_eq!(
            GameStateName::ALL.map(GameStateName::name),
            ["menu", "playing", "paused", "game_over"]
        );
        assert_eq!(GameStateName::from_name("Paused"), None);
        assert_eq!(
            GameStateName::from_name("game_over"),
            Some(GameStateName::GameOver)
        );
        assert_eq!(GameStateName::default(), GameStateName::Playing);
        assert_eq!(GameState::default().current, GameStateName::Playing);
        assert_eq!(GameStateName::GameOver.label(), "Game over");
    }

    #[test]
    fn environment_with_start_state_is_not_empty() {
        let mut environment = crate::EnvironmentData::default();
        assert!(environment.is_empty());
        environment.start_state = Some(GameStateName::Menu);
        assert!(!environment.is_empty());
    }
}
