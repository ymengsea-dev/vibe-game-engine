//! Local avoidance: walking agents steer round each other.
//!
//! Each frame every walking agent has a *preferred* velocity (where its path
//! wants it to go). [`avoid`] picks, for each one, the velocity closest to
//! that preference that does not run into a neighbour soon, assuming each
//! neighbour does its half of the dodging (reciprocal velocity obstacles,
//! solved by sampling candidate velocities). Agents that stand still are
//! obstacles that never move out of the way.
//!
//! Pure maths on the ground plane, like [`crate::nav`]: the caller turns the
//! chosen velocities into moves and keeps them on walkable ground.

use glam::Vec2;

/// How far ahead, in seconds, a collision counts.
pub const AVOID_HORIZON: f32 = 1.5;
/// Neighbours further than this many of the agent's own steps per horizon
/// are ignored.
const NEIGHBOUR_REACH: f32 = 2.0;
/// The most neighbours one agent considers, nearest first.
const MAX_NEIGHBOURS: usize = 10;
/// Directions sampled around the agent.
const DIRECTIONS: usize = 24;
/// Speeds sampled per direction, as shares of the agent's speed.
const SPEED_SHARES: [f32; 3] = [1.0, 0.66, 0.33];
/// How much a near collision weighs against straying from the preference.
const COLLISION_WEIGHT: f32 = 2.0;

/// One agent on the ground plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvoidAgent {
    /// Where it is.
    pub position: Vec2,
    /// The velocity it moved with last frame (what its neighbours expect).
    pub velocity: Vec2,
    /// The velocity its path wants now; zero for an agent standing still.
    pub preferred: Vec2,
    /// Its size, as a circle.
    pub radius: f32,
    /// Its top speed.
    pub max_speed: f32,
    /// Whether it steers at all. One that doesn't still counts as an
    /// obstacle for the others, moving as it likes.
    pub steers: bool,
}

/// A new velocity for every agent, in the same order. Agents that don't
/// steer keep their preferred velocity.
pub fn avoid(agents: &[AvoidAgent]) -> Vec<Vec2> {
    agents
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            if !agent.steers || agent.max_speed <= 0.0 || !agent.preferred.is_finite() {
                return agent.preferred;
            }
            let neighbours = neighbours(agents, index);
            if neighbours.is_empty() {
                return agent.preferred;
            }
            best_velocity(agent, &neighbours)
        })
        .collect()
}

/// The agents near `agents[index]` that could matter within the horizon,
/// nearest first.
fn neighbours(agents: &[AvoidAgent], index: usize) -> Vec<&AvoidAgent> {
    let agent = &agents[index];
    let mut near: Vec<(f32, &AvoidAgent)> = agents
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .filter_map(|(_, other)| {
            let reach = (agent.max_speed + other.max_speed) * AVOID_HORIZON * NEIGHBOUR_REACH
                + agent.radius
                + other.radius;
            let distance = agent.position.distance(other.position);
            (distance.is_finite() && distance <= reach).then_some((distance, other))
        })
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near.truncate(MAX_NEIGHBOURS);
    near.into_iter().map(|(_, other)| other).collect()
}

/// The sampled velocity with the lowest cost: how far it strays from the
/// preference plus how soon it would hit someone.
fn best_velocity(agent: &AvoidAgent, neighbours: &[&AvoidAgent]) -> Vec2 {
    let mut best = agent.preferred;
    let mut best_cost = cost(agent, agent.preferred, neighbours);
    let mut candidates = Vec::with_capacity(DIRECTIONS * SPEED_SHARES.len() + 1);
    candidates.push(Vec2::ZERO);
    for step in 0..DIRECTIONS {
        let angle = step as f32 / DIRECTIONS as f32 * std::f32::consts::TAU;
        let direction = Vec2::from_angle(angle);
        for share in SPEED_SHARES {
            candidates.push(direction * agent.max_speed * share);
        }
    }
    for candidate in candidates {
        let cost = cost(agent, candidate, neighbours);
        if cost < best_cost {
            best_cost = cost;
            best = candidate;
        }
    }
    best
}

fn cost(agent: &AvoidAgent, candidate: Vec2, neighbours: &[&AvoidAgent]) -> f32 {
    let stray = candidate.distance(agent.preferred);
    let soonest = neighbours
        .iter()
        .map(|other| {
            // Reciprocal: a moving neighbour dodges half, so this agent's
            // velocity is judged against the halfway point between its own
            // old velocity and the candidate. A standing one doesn't dodge.
            let relative = if other.preferred == Vec2::ZERO && other.velocity == Vec2::ZERO {
                candidate
            } else {
                2.0 * candidate - agent.velocity - other.velocity
            };
            time_to_collision(
                other.position - agent.position,
                relative,
                agent.radius + other.radius,
            )
        })
        .fold(f32::INFINITY, f32::min);
    if soonest <= 0.0 {
        // Already touching: only moving apart is any good.
        return f32::MAX / 4.0 + stray;
    }
    let danger = if soonest < AVOID_HORIZON {
        COLLISION_WEIGHT * agent.max_speed * (AVOID_HORIZON / soonest)
    } else {
        0.0
    };
    stray + danger
}

/// Seconds until two circles `radius` apart (summed) meet, when one is at
/// `offset` from the other and they close at `relative` velocity. Infinity
/// when they never meet; `0` when they overlap and are not moving apart.
fn time_to_collision(offset: Vec2, relative: Vec2, radius: f32) -> f32 {
    let distance_sq = offset.length_squared();
    let radius_sq = radius * radius;
    if distance_sq < radius_sq {
        return if relative.dot(offset) > 0.0 {
            0.0
        } else {
            f32::INFINITY
        };
    }
    let a = relative.length_squared();
    if a <= f32::EPSILON {
        return f32::INFINITY;
    }
    let b = relative.dot(offset);
    if b <= 0.0 {
        return f32::INFINITY;
    }
    let discriminant = b * b - a * (distance_sq - radius_sq);
    if discriminant < 0.0 {
        return f32::INFINITY;
    }
    let time = (b - discriminant.sqrt()) / a;
    if time >= 0.0 { time } else { f32::INFINITY }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walker(position: Vec2, goal: Vec2) -> AvoidAgent {
        let preferred = (goal - position).normalize_or_zero() * 2.0;
        AvoidAgent {
            position,
            velocity: preferred,
            preferred,
            radius: 0.4,
            max_speed: 2.0,
            steers: true,
        }
    }

    /// Steps every agent toward its goal for `seconds`; returns the closest
    /// any two came.
    fn run(agents: &mut [AvoidAgent], goals: &[Vec2], seconds: f32) -> f32 {
        let dt = 1.0 / 30.0;
        let mut closest = f32::INFINITY;
        for _ in 0..(seconds / dt) as usize {
            for (agent, goal) in agents.iter_mut().zip(goals) {
                let to_goal = *goal - agent.position;
                agent.preferred = if to_goal.length() < 0.1 {
                    Vec2::ZERO
                } else {
                    to_goal.normalize() * agent.max_speed.min(to_goal.length() / dt)
                };
            }
            let velocities = avoid(agents);
            for (agent, velocity) in agents.iter_mut().zip(velocities) {
                agent.velocity = velocity;
                agent.position += velocity * dt;
            }
            for (i, a) in agents.iter().enumerate() {
                for b in &agents[i + 1..] {
                    closest = closest.min(a.position.distance(b.position));
                }
            }
        }
        closest
    }

    #[test]
    fn two_agents_walking_head_on_pass_without_touching() {
        let goals = [Vec2::new(10.0, 0.0), Vec2::new(-10.0, 0.0)];
        let mut agents = [
            walker(Vec2::new(-10.0, 0.0), goals[0]),
            walker(Vec2::new(10.0, 0.0), goals[1]),
        ];
        let closest = run(&mut agents, &goals, 15.0);
        assert!(closest >= 0.75, "came within {closest}");
        for (agent, goal) in agents.iter().zip(goals) {
            assert!(agent.position.distance(goal) < 0.3, "reached {goal}");
        }
    }

    #[test]
    fn a_crowd_crossing_reaches_its_goals_without_overlapping() {
        let mut agents = Vec::new();
        let mut goals = Vec::new();
        for i in 0..10 {
            let y = i as f32 * 1.2 - 6.0;
            agents.push(walker(Vec2::new(-8.0, y), Vec2::new(8.0, -y)));
            goals.push(Vec2::new(8.0, -y));
        }
        let closest = run(&mut agents, &goals, 30.0);
        assert!(closest >= 0.6, "came within {closest}");
        let arrived = agents
            .iter()
            .zip(&goals)
            .filter(|(agent, goal)| agent.position.distance(**goal) < 0.5)
            .count();
        assert!(arrived >= 9, "{arrived} of 10 arrived");
    }

    #[test]
    fn an_agent_walks_round_one_standing_still() {
        let goal = Vec2::new(10.0, 0.0);
        let mut agents = [
            walker(Vec2::new(-5.0, 0.0), goal),
            AvoidAgent {
                position: Vec2::ZERO,
                velocity: Vec2::ZERO,
                preferred: Vec2::ZERO,
                radius: 0.4,
                max_speed: 2.0,
                steers: false,
            },
        ];
        let goals = [goal, Vec2::ZERO];
        let closest = run(&mut agents, &goals, 10.0);
        assert!(closest >= 0.75, "came within {closest}");
        assert!(agents[0].position.distance(goal) < 0.3);
    }

    #[test]
    fn agents_that_do_not_steer_keep_their_path() {
        let mut agent = walker(Vec2::ZERO, Vec2::new(1.0, 0.0));
        agent.steers = false;
        let other = walker(Vec2::new(1.0, 0.0), Vec2::ZERO);
        assert_eq!(avoid(&[agent, other])[0], agent.preferred);
    }
}
