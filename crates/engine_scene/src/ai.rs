//! In-game AI settings saved on objects: how an object walks the scene's
//! nav grid, what it senses, and the brain that decides what it does.
//!
//! Everything lives in one [`AiData`] on the object (like the animator on a
//! model), so it saves, undoes, copies and travels in prefabs with the
//! object. `engine::gameplay` runs it; this module is the format, its
//! limits and its checks.

use serde::{Deserialize, Serialize};

/// The most patrol points one brain may have.
pub const MAX_PATROL_POINTS: usize = 64;
/// The longest a target tag or event name may be, in bytes.
pub const MAX_AI_NAME_BYTES: usize = 64;
/// The largest distance any AI setting may use, in world units.
pub const MAX_AI_DISTANCE: f32 = 10_000.0;

/// How Studio builds a nav grid from the scene.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NavBakeSettings {
    /// Side of one cell, in world units. Smaller finds narrower gaps but
    /// makes a bigger grid.
    #[serde(default = "default_cell_size")]
    pub cell_size: f32,
    /// How far walking characters keep from walls.
    #[serde(default = "default_agent_radius")]
    pub agent_radius: f32,
    /// How tall walking characters are: anything lower overhead blocks.
    #[serde(default = "default_agent_height")]
    pub agent_height: f32,
    /// The highest ledge characters step up without it counting as a wall.
    #[serde(default = "default_step_height")]
    pub step_height: f32,
    /// The steepest ground characters walk on, in degrees.
    #[serde(default = "default_max_slope")]
    pub max_slope_degrees: f32,
}

fn default_cell_size() -> f32 {
    0.5
}
fn default_agent_radius() -> f32 {
    0.4
}
fn default_agent_height() -> f32 {
    1.8
}
fn default_step_height() -> f32 {
    0.4
}
fn default_max_slope() -> f32 {
    40.0
}

impl Default for NavBakeSettings {
    fn default() -> Self {
        Self {
            cell_size: default_cell_size(),
            agent_radius: default_agent_radius(),
            agent_height: default_agent_height(),
            step_height: default_step_height(),
            max_slope_degrees: default_max_slope(),
        }
    }
}

impl NavBakeSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        in_range("cell size", self.cell_size, 0.05, 100.0)?;
        in_range("agent radius", self.agent_radius, 0.0, 100.0)?;
        in_range("agent height", self.agent_height, 0.01, 100.0)?;
        in_range("step height", self.step_height, 0.0, 100.0)?;
        in_range("max slope", self.max_slope_degrees, 0.0, 89.0)
    }
}

/// Everything AI on one object.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AiData {
    /// How it walks.
    #[serde(default)]
    pub agent: NavAgentData,
    /// What it notices, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub senses: Option<SensesData>,
    /// What decides its actions, if anything. Without one it only walks
    /// where scripts and rules send it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brain: Option<AiBrainData>,
}

/// The live copy of an object's [`AiData`], so Studio can show and re-save it
/// and the game can run it.
#[derive(bevy_ecs::component::Component, Debug, Clone, PartialEq)]
pub struct SceneAi(pub AiData);

/// How an object walks along routes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NavAgentData {
    /// Walking speed, in world units per second.
    #[serde(default = "default_walk_speed")]
    pub speed: f32,
    /// How close to its destination counts as arrived.
    #[serde(default = "default_stopping_distance")]
    pub stopping_distance: f32,
    /// How fast it turns to face where it walks, in degrees per second.
    /// `0` never turns it.
    #[serde(default = "default_turn_speed")]
    pub turn_speed: f32,
    /// Its size as a circle on the ground, for steering round others.
    #[serde(
        default = "default_agent_radius",
        skip_serializing_if = "is_default_agent_radius"
    )]
    pub radius: f32,
    /// Whether it steers round other AI objects while walking. Off, it
    /// keeps to its path (others still steer round it).
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub avoid_others: bool,
}

/// Largest agent radius.
pub const MAX_AGENT_RADIUS: f32 = 10.0;

fn is_default_agent_radius(radius: &f32) -> bool {
    *radius == default_agent_radius()
}
fn default_true() -> bool {
    true
}
fn is_true(value: &bool) -> bool {
    *value
}

fn default_walk_speed() -> f32 {
    3.0
}
fn default_stopping_distance() -> f32 {
    0.2
}
fn default_turn_speed() -> f32 {
    540.0
}

impl Default for NavAgentData {
    fn default() -> Self {
        Self {
            speed: default_walk_speed(),
            stopping_distance: default_stopping_distance(),
            turn_speed: default_turn_speed(),
            radius: default_agent_radius(),
            avoid_others: true,
        }
    }
}

/// What an object notices: targets it sees or hears, and noises.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SensesData {
    /// The tag of the objects it looks for, such as `player`.
    #[serde(default = "default_target_tag")]
    pub target_tag: String,
    /// How far it sees.
    #[serde(default = "default_sight_range")]
    pub sight_range: f32,
    /// How wide it sees, in degrees across (`360` sees all round).
    #[serde(default = "default_sight_angle")]
    pub sight_angle: f32,
    /// Height of its eyes above its position; walls between its eyes and a
    /// target block the view.
    #[serde(default = "default_eye_height")]
    pub eye_height: f32,
    /// Targets and noises inside this distance are heard, through walls.
    #[serde(default = "default_hearing_radius")]
    pub hearing_radius: f32,
    /// Seconds without seeing or hearing a target before it counts as lost.
    #[serde(default = "default_forget_after")]
    pub forget_after: f32,
}

fn default_target_tag() -> String {
    "player".to_owned()
}
fn default_sight_range() -> f32 {
    12.0
}
fn default_sight_angle() -> f32 {
    120.0
}
fn default_eye_height() -> f32 {
    1.6
}
fn default_hearing_radius() -> f32 {
    3.0
}
fn default_forget_after() -> f32 {
    3.0
}

impl Default for SensesData {
    fn default() -> Self {
        Self {
            target_tag: default_target_tag(),
            sight_range: default_sight_range(),
            sight_angle: default_sight_angle(),
            eye_height: default_eye_height(),
            hearing_radius: default_hearing_radius(),
            forget_after: default_forget_after(),
        }
    }
}

/// What decides an object's actions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AiBrainData {
    /// Ready-made idle, patrol, chase and attack.
    Simple(SimpleBrainData),
    /// A behaviour tree made in the node editor.
    Tree(crate::behaviour_tree::BehaviourTreeData),
}

/// The four states of a [`SimpleBrainData`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum AiState {
    /// Stands still.
    #[default]
    Idle,
    /// Walks between its patrol points.
    Patrol,
    /// Runs after its target.
    Chase,
    /// Is close enough to its target to attack it.
    Attack,
}

impl AiState {
    /// Every state, in the order Studio lists them.
    pub const ALL: [AiState; 4] = [Self::Idle, Self::Patrol, Self::Chase, Self::Attack];

    /// The name scripts and rules use: `idle`, `patrol`, `chase`, `attack`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Patrol => "patrol",
            Self::Chase => "chase",
            Self::Attack => "attack",
        }
    }

    /// The state called `name`, ignoring case.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|state| state.name().eq_ignore_ascii_case(name))
    }
}

/// Ready-made enemy behaviour: idle or patrol until a target is seen or
/// heard, chase it, attack it when close, and go back once it is lost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimpleBrainData {
    /// What it does before it notices anything: [`AiState::Idle`] or
    /// [`AiState::Patrol`].
    #[serde(default)]
    pub start: AiState,
    /// World positions it walks between while patrolling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub patrol_points: Vec<[f32; 3]>,
    /// Seconds it waits at each patrol point.
    #[serde(default = "default_patrol_wait")]
    pub patrol_wait: f32,
    /// After the last point, walk back through them in reverse instead of
    /// going straight to the first.
    #[serde(default)]
    pub patrol_back_and_forth: bool,
    /// Running speed while chasing, in world units per second.
    #[serde(default = "default_chase_speed")]
    pub chase_speed: f32,
    /// How close the target must be to attack it.
    #[serde(default = "default_attack_range")]
    pub attack_range: f32,
    /// Seconds between attacks.
    #[serde(default = "default_attack_cooldown")]
    pub attack_cooldown: f32,
    /// The event each attack sends, for rules and scripts to act on.
    #[serde(default = "default_attack_event")]
    pub attack_event: String,
    /// Clips an animated model plays in each state.
    #[serde(default, skip_serializing_if = "AiClips::is_empty")]
    pub clips: AiClips,
}

fn default_patrol_wait() -> f32 {
    1.0
}
fn default_chase_speed() -> f32 {
    5.0
}
fn default_attack_range() -> f32 {
    1.5
}
fn default_attack_cooldown() -> f32 {
    1.0
}
fn default_attack_event() -> String {
    "attack".to_owned()
}

impl Default for SimpleBrainData {
    fn default() -> Self {
        Self {
            start: AiState::Idle,
            patrol_points: Vec::new(),
            patrol_wait: default_patrol_wait(),
            patrol_back_and_forth: false,
            chase_speed: default_chase_speed(),
            attack_range: default_attack_range(),
            attack_cooldown: default_attack_cooldown(),
            attack_event: default_attack_event(),
            clips: AiClips::default(),
        }
    }
}

/// Clip names for each state of a [`SimpleBrainData`]; empty plays nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AiClips {
    /// Played while idle and while waiting at a patrol point.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub idle: String,
    /// Played while patrolling.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub walk: String,
    /// Played while chasing.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run: String,
    /// Played on each attack.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub attack: String,
}

impl AiClips {
    /// Whether no state plays a clip.
    pub fn is_empty(&self) -> bool {
        self.idle.is_empty()
            && self.walk.is_empty()
            && self.run.is_empty()
            && self.attack.is_empty()
    }

    /// The clip for `state`, if one is set.
    pub fn for_state(&self, state: AiState) -> Option<&str> {
        let clip = match state {
            AiState::Idle => &self.idle,
            AiState::Patrol => &self.walk,
            AiState::Chase => &self.run,
            AiState::Attack => &self.attack,
        };
        (!clip.is_empty()).then_some(clip.as_str())
    }
}

impl AiData {
    /// Checks every number is in range and every name is short and printable.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let agent = &self.agent;
        in_range("speed", agent.speed, 0.0, MAX_AI_DISTANCE)?;
        in_range(
            "stopping distance",
            agent.stopping_distance,
            0.0,
            MAX_AI_DISTANCE,
        )?;
        in_range("turn speed", agent.turn_speed, 0.0, 100_000.0)?;
        in_range("radius", agent.radius, 0.0, MAX_AGENT_RADIUS)?;
        if let Some(senses) = &self.senses {
            valid_ai_name("target tag", &senses.target_tag)?;
            in_range("sight range", senses.sight_range, 0.0, MAX_AI_DISTANCE)?;
            in_range("sight angle", senses.sight_angle, 0.0, 360.0)?;
            in_range(
                "eye height",
                senses.eye_height,
                -MAX_AI_DISTANCE,
                MAX_AI_DISTANCE,
            )?;
            in_range(
                "hearing radius",
                senses.hearing_radius,
                0.0,
                MAX_AI_DISTANCE,
            )?;
            in_range("forget after", senses.forget_after, 0.0, 3_600.0)?;
        }
        match &self.brain {
            None => Ok(()),
            Some(AiBrainData::Simple(brain)) => brain.validate(),
            Some(AiBrainData::Tree(tree)) => tree.validate(),
        }
    }
}

impl SimpleBrainData {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.start, AiState::Idle | AiState::Patrol) {
            return Err("a brain starts idle or patrolling".to_owned());
        }
        if self.patrol_points.len() > MAX_PATROL_POINTS {
            return Err(format!("more than {MAX_PATROL_POINTS} patrol points"));
        }
        if !self
            .patrol_points
            .iter()
            .flatten()
            .all(|value| value.is_finite() && value.abs() <= MAX_AI_DISTANCE * 10.0)
        {
            return Err("a patrol point is not a usable position".to_owned());
        }
        in_range("patrol wait", self.patrol_wait, 0.0, 3_600.0)?;
        in_range("chase speed", self.chase_speed, 0.0, MAX_AI_DISTANCE)?;
        in_range("attack range", self.attack_range, 0.0, MAX_AI_DISTANCE)?;
        in_range("attack cooldown", self.attack_cooldown, 0.0, 3_600.0)?;
        if !self.attack_event.is_empty() {
            valid_ai_name("attack event", &self.attack_event)?;
        }
        for clip in [
            &self.clips.idle,
            &self.clips.walk,
            &self.clips.run,
            &self.clips.attack,
        ] {
            if clip.len() > 256 || clip.chars().any(char::is_control) {
                return Err("a clip name is too long or not printable".to_owned());
            }
        }
        Ok(())
    }
}

/// Checks `value` is finite and within `min..=max`.
pub(crate) fn in_range(what: &str, value: f32, min: f32, max: f32) -> Result<(), String> {
    if value.is_finite() && (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "{what} must be between {min} and {max}, got {value}"
        ))
    }
}

/// Checks a tag or event name: 1 to [`MAX_AI_NAME_BYTES`] printable bytes.
pub(crate) fn valid_ai_name(what: &str, name: &str) -> Result<(), String> {
    if name.trim().is_empty()
        || name.len() > MAX_AI_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        Err(format!(
            "{what} must be 1 to {MAX_AI_NAME_BYTES} printable bytes"
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_round_trip() {
        let data = AiData {
            agent: NavAgentData::default(),
            senses: Some(SensesData::default()),
            brain: Some(AiBrainData::Simple(SimpleBrainData {
                start: AiState::Patrol,
                patrol_points: vec![[0.0, 0.0, 0.0], [4.0, 0.0, 2.0]],
                ..SimpleBrainData::default()
            })),
        };
        data.validate().expect("defaults are valid");
        let text = ron::to_string(&data).expect("serialises");
        let back: AiData = ron::from_str(&text).expect("parses");
        assert_eq!(back, data);
    }

    #[test]
    fn an_empty_record_reads_as_every_default() {
        let data: AiData = ron::from_str("(senses: Some(()), brain: Some(Simple(())))")
            .expect("all fields default");
        assert_eq!(data.agent, NavAgentData::default());
        assert_eq!(data.senses, Some(SensesData::default()));
        assert_eq!(
            data.brain,
            Some(AiBrainData::Simple(SimpleBrainData::default()))
        );
    }

    #[test]
    fn bad_values_are_rejected() {
        let mut data = AiData::default();
        data.agent.speed = f32::NAN;
        assert!(data.validate().is_err());

        let mut data = AiData {
            senses: Some(SensesData {
                target_tag: String::new(),
                ..SensesData::default()
            }),
            ..AiData::default()
        };
        assert!(data.validate().is_err());
        data.senses = Some(SensesData {
            sight_angle: 400.0,
            ..SensesData::default()
        });
        assert!(data.validate().is_err());

        let brain = |brain: SimpleBrainData| AiData {
            brain: Some(AiBrainData::Simple(brain)),
            ..AiData::default()
        };
        assert!(
            brain(SimpleBrainData {
                start: AiState::Chase,
                ..SimpleBrainData::default()
            })
            .validate()
            .is_err()
        );
        assert!(
            brain(SimpleBrainData {
                patrol_points: vec![[0.0; 3]; MAX_PATROL_POINTS + 1],
                ..SimpleBrainData::default()
            })
            .validate()
            .is_err()
        );
        assert!(
            brain(SimpleBrainData {
                patrol_points: vec![[f32::INFINITY, 0.0, 0.0]],
                ..SimpleBrainData::default()
            })
            .validate()
            .is_err()
        );
    }

    #[test]
    fn state_names_round_trip_ignoring_case() {
        for state in AiState::ALL {
            assert_eq!(AiState::from_name(state.name()), Some(state));
        }
        assert_eq!(AiState::from_name("CHASE"), Some(AiState::Chase));
        assert_eq!(AiState::from_name("sleep"), None);
    }

    #[test]
    fn bake_settings_reject_tiny_cells_and_steep_slopes() {
        assert!(NavBakeSettings::default().validate().is_ok());
        let tiny = NavBakeSettings {
            cell_size: 0.0,
            ..NavBakeSettings::default()
        };
        assert!(tiny.validate().is_err());
        let steep = NavBakeSettings {
            max_slope_degrees: 90.0,
            ..NavBakeSettings::default()
        };
        assert!(steep.validate().is_err());
    }
}
