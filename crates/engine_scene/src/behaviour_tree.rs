//! Behaviour trees: an AI brain made of nodes in Studio's node editor.
//!
//! A tree is run from its root every frame. Composite nodes run their
//! children in order: a **Sequence** stops at the first child that fails,
//! a **Selector** stops at the first that succeeds. Condition nodes check
//! something now (can it see its target?) and action nodes do something
//! (walk to the target, wait). An action that takes time, such as walking,
//! answers "still running" until it is done, and the tree comes back to it
//! next frame.
//!
//! The tree is saved on the object, in its [`crate::AiData`], like the
//! animator. Node `0` is the root.

use serde::{Deserialize, Serialize};

use crate::format::RuleCondition;

/// The most nodes one tree may have.
pub const MAX_BT_NODES: usize = 128;
/// The most children one node may have.
pub const MAX_BT_CHILDREN: usize = 16;

/// What a node answered on the last frame it ran, for Studio's live view of
/// a running tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BtNodeRun {
    /// It worked.
    Success,
    /// It did not work.
    Failure,
    /// It is still going (walking, waiting).
    Running,
}

/// The nodes one frame of a tree visited, in the order they ran, with what
/// each answered.
pub type BtTrace = Vec<(u16, BtNodeRun)>;

/// A behaviour tree. Node `0` is the root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviourTreeData {
    /// Every node; each child index points into this list.
    pub nodes: Vec<BtNodeData>,
}

/// One node: what it is, its children in run order, and where Studio
/// draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BtNodeData {
    /// What the node does.
    pub kind: BtNodeKind,
    /// Indices of its children, run from first to last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<usize>,
    /// Where the node editor draws it; editor layout only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_position: Option<[f32; 2]>,
}

impl BtNodeData {
    /// A node of `kind` with no children.
    pub fn new(kind: BtNodeKind) -> Self {
        Self {
            kind,
            children: Vec::new(),
            graph_position: None,
        }
    }
}

/// What a behaviour-tree node does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BtNodeKind {
    /// Runs children in order until one fails; succeeds when all do.
    Sequence,
    /// Runs children in order until one succeeds; fails when all do.
    Selector,
    /// Runs its one child and flips success and failure.
    Inverter,
    /// Succeeds while the object sees its target.
    CanSeeTarget,
    /// Succeeds while the object hears its target.
    CanHearTarget,
    /// Succeeds while the object has a target it has not lost yet.
    HasTarget,
    /// Succeeds while its target is within `distance`.
    TargetWithin {
        /// World units.
        distance: f32,
    },
    /// Succeeds while a game variable compares as set.
    Variable(RuleCondition),
    /// Succeeds `percent` per cent of the times it runs.
    Chance {
        /// `0` to `100`.
        percent: f32,
    },
    /// Walks toward the target until within the stopping distance.
    MoveToTarget {
        /// Speed while doing it, or the agent's speed when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speed: Option<f32>,
    },
    /// Walks to the next patrol point; succeeds on arrival.
    Patrol {
        /// World positions, visited in order and then from the first again.
        points: Vec<[f32; 3]>,
    },
    /// Walks to a fixed world position.
    MoveTo {
        /// Where to go.
        position: [f32; 3],
    },
    /// Waits, then succeeds.
    Wait {
        /// How long, in seconds.
        seconds: f32,
    },
    /// Plays an animation clip on the object and succeeds.
    PlayAnimation {
        /// The clip's name in the model file.
        clip: String,
    },
    /// Sends a named event to rules and scripts and succeeds.
    SendEvent {
        /// The event's name.
        name: String,
    },
    /// Sets a game variable and succeeds.
    SetVariable {
        /// Variable name.
        name: String,
        /// New value.
        value: f64,
    },
    /// Turns to face the target; fails without one.
    FaceTarget,
    /// Stops walking and succeeds.
    Stop,
}

impl BtNodeKind {
    /// Whether this node runs children.
    pub fn is_composite(&self) -> bool {
        matches!(self, Self::Sequence | Self::Selector | Self::Inverter)
    }

    /// The most children this node may have.
    pub fn max_children(&self) -> usize {
        match self {
            Self::Sequence | Self::Selector => MAX_BT_CHILDREN,
            Self::Inverter => 1,
            _ => 0,
        }
    }

    /// The short name Studio shows on the node.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Sequence => "Sequence",
            Self::Selector => "Selector",
            Self::Inverter => "Not",
            Self::CanSeeTarget => "Can see target?",
            Self::CanHearTarget => "Can hear target?",
            Self::HasTarget => "Has target?",
            Self::TargetWithin { .. } => "Target within?",
            Self::Variable(_) => "Variable?",
            Self::Chance { .. } => "Chance?",
            Self::MoveToTarget { .. } => "Move to target",
            Self::Patrol { .. } => "Patrol",
            Self::MoveTo { .. } => "Move to",
            Self::Wait { .. } => "Wait",
            Self::PlayAnimation { .. } => "Play animation",
            Self::SendEvent { .. } => "Send event",
            Self::SetVariable { .. } => "Set variable",
            Self::FaceTarget => "Face target",
            Self::Stop => "Stop",
        }
    }

    /// One node of every kind, with default settings, in the order Studio's
    /// "Add" menu lists them.
    pub fn palette() -> Vec<BtNodeKind> {
        vec![
            Self::Sequence,
            Self::Selector,
            Self::Inverter,
            Self::CanSeeTarget,
            Self::CanHearTarget,
            Self::HasTarget,
            Self::TargetWithin { distance: 2.0 },
            Self::Variable(RuleCondition {
                variable: "health".to_owned(),
                compare: crate::format::RuleCompare::Greater,
                value: 0.0,
            }),
            Self::Chance { percent: 50.0 },
            Self::MoveToTarget { speed: None },
            Self::Patrol { points: Vec::new() },
            Self::MoveTo { position: [0.0; 3] },
            Self::Wait { seconds: 1.0 },
            Self::PlayAnimation {
                clip: String::new(),
            },
            Self::SendEvent {
                name: "attack".to_owned(),
            },
            Self::SetVariable {
                name: "alert".to_owned(),
                value: 1.0,
            },
            Self::FaceTarget,
            Self::Stop,
        ]
    }
}

impl Default for BehaviourTreeData {
    /// The starter tree: chase a seen target, otherwise wait.
    fn default() -> Self {
        let mut chase = BtNodeData::new(BtNodeKind::Sequence);
        chase.children = vec![2, 3];
        let mut root = BtNodeData::new(BtNodeKind::Selector);
        root.children = vec![1, 4];
        Self {
            nodes: vec![
                root,
                chase,
                BtNodeData::new(BtNodeKind::HasTarget),
                BtNodeData::new(BtNodeKind::MoveToTarget { speed: None }),
                BtNodeData::new(BtNodeKind::Wait { seconds: 1.0 }),
            ],
        }
    }
}

impl BehaviourTreeData {
    /// Checks the tree is a tree: a root, every child index in range, no node
    /// with two parents or that is its own ancestor, no node unreachable from
    /// the root, children only where the kind takes them, and every setting
    /// in range.
    pub fn validate(&self) -> Result<(), String> {
        if self.nodes.is_empty() {
            return Err("a behaviour tree needs a root node".to_owned());
        }
        if self.nodes.len() > MAX_BT_NODES {
            return Err(format!(
                "a behaviour tree may have at most {MAX_BT_NODES} nodes"
            ));
        }
        let mut parent = vec![None; self.nodes.len()];
        for (index, node) in self.nodes.iter().enumerate() {
            if node.children.len() > node.kind.max_children() {
                return Err(format!(
                    "node {index} ({}) may have at most {} children",
                    node.kind.label(),
                    node.kind.max_children()
                ));
            }
            for &child in &node.children {
                if child >= self.nodes.len() {
                    return Err(format!("node {index} has a child that does not exist"));
                }
                if child == 0 || parent[child].is_some() {
                    return Err(format!("node {child} has more than one parent"));
                }
                parent[child] = Some(index);
            }
            validate_kind(&node.kind).map_err(|reason| format!("node {index}: {reason}"))?;
        }
        // One parent each and the root has none, so the only way a node can
        // be cut off is a loop of nodes that are each other's parents.
        if let Some(lost) = (1..self.nodes.len()).find(|&index| {
            let mut at = index;
            let mut steps = 0;
            while let Some(up) = parent[at] {
                at = up;
                steps += 1;
                if steps > self.nodes.len() {
                    return true;
                }
            }
            at != 0
        }) {
            return Err(format!("node {lost} is not connected to the root"));
        }
        Ok(())
    }

    /// The parent of node `index`, if it has one.
    pub fn parent_of(&self, index: usize) -> Option<usize> {
        self.nodes
            .iter()
            .position(|node| node.children.contains(&index))
    }

    /// Removes node `index` and everything under it, fixing every child index
    /// that pointed past it. The root cannot be removed.
    pub fn remove_subtree(&mut self, index: usize) {
        if index == 0 || index >= self.nodes.len() {
            return;
        }
        let mut doomed = vec![false; self.nodes.len()];
        let mut stack = vec![index];
        while let Some(at) = stack.pop() {
            if doomed[at] {
                continue;
            }
            doomed[at] = true;
            stack.extend(self.nodes[at].children.iter().copied());
        }
        let mut remap = vec![usize::MAX; self.nodes.len()];
        let mut next = 0;
        for (old, gone) in doomed.iter().enumerate() {
            if !gone {
                remap[old] = next;
                next += 1;
            }
        }
        let mut kept = Vec::with_capacity(next);
        for (old, mut node) in std::mem::take(&mut self.nodes).into_iter().enumerate() {
            if doomed[old] {
                continue;
            }
            node.children = node
                .children
                .iter()
                .filter(|&&child| !doomed[child])
                .map(|&child| remap[child])
                .collect();
            kept.push(node);
        }
        self.nodes = kept;
    }
}

fn validate_kind(kind: &BtNodeKind) -> Result<(), String> {
    use crate::ai::{MAX_AI_DISTANCE, MAX_PATROL_POINTS, in_range, valid_ai_name};
    match kind {
        BtNodeKind::TargetWithin { distance } => {
            in_range("distance", *distance, 0.0, MAX_AI_DISTANCE)
        }
        BtNodeKind::Variable(condition) => {
            if crate::rules::valid_variable_name(&condition.variable) && condition.value.is_finite()
            {
                Ok(())
            } else {
                Err("needs a variable name and a finite value".to_owned())
            }
        }
        BtNodeKind::Chance { percent } => in_range("chance", *percent, 0.0, 100.0),
        BtNodeKind::MoveToTarget { speed: Some(speed) } => {
            in_range("speed", *speed, 0.0, MAX_AI_DISTANCE)
        }
        BtNodeKind::Patrol { points } => {
            if points.len() > MAX_PATROL_POINTS {
                return Err(format!("more than {MAX_PATROL_POINTS} patrol points"));
            }
            if points.iter().flatten().all(|value| value.is_finite()) {
                Ok(())
            } else {
                Err("a patrol point is not a usable position".to_owned())
            }
        }
        BtNodeKind::MoveTo { position } => {
            if position.iter().all(|value| value.is_finite()) {
                Ok(())
            } else {
                Err("the position is not usable".to_owned())
            }
        }
        BtNodeKind::Wait { seconds } => in_range("wait", *seconds, 0.0, 3_600.0),
        BtNodeKind::PlayAnimation { clip } => {
            if clip.len() > 256 || clip.chars().any(char::is_control) {
                Err("the clip name is too long or not printable".to_owned())
            } else {
                Ok(())
            }
        }
        BtNodeKind::SendEvent { name } => valid_ai_name("event", name),
        BtNodeKind::SetVariable { name, value } => {
            if crate::rules::valid_variable_name(name) && value.is_finite() {
                Ok(())
            } else {
                Err("needs a variable name and a finite value".to_owned())
            }
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starter_tree_is_valid_and_round_trips() {
        let tree = BehaviourTreeData::default();
        tree.validate().expect("valid");
        let text = ron::to_string(&tree).expect("serialises");
        assert_eq!(ron::from_str::<BehaviourTreeData>(&text).ok(), Some(tree));
        for kind in BtNodeKind::palette() {
            assert!(validate_kind(&kind).is_ok(), "{kind:?}");
        }
    }

    #[test]
    fn shapes_that_are_not_trees_are_rejected() {
        let node = |kind, children: Vec<usize>| BtNodeData {
            kind,
            children,
            graph_position: None,
        };
        let empty = BehaviourTreeData { nodes: Vec::new() };
        assert!(empty.validate().is_err());

        let two_parents = BehaviourTreeData {
            nodes: vec![
                node(BtNodeKind::Sequence, vec![1, 2]),
                node(BtNodeKind::Selector, vec![2]),
                node(BtNodeKind::Stop, vec![]),
            ],
        };
        assert!(two_parents.validate().is_err());

        let out_of_range = BehaviourTreeData {
            nodes: vec![node(BtNodeKind::Sequence, vec![5])],
        };
        assert!(out_of_range.validate().is_err());

        let back_to_root = BehaviourTreeData {
            nodes: vec![
                node(BtNodeKind::Sequence, vec![1]),
                node(BtNodeKind::Sequence, vec![0]),
            ],
        };
        assert!(back_to_root.validate().is_err());

        let loop_off_the_root = BehaviourTreeData {
            nodes: vec![
                node(BtNodeKind::Sequence, vec![]),
                node(BtNodeKind::Sequence, vec![2]),
                node(BtNodeKind::Sequence, vec![1]),
            ],
        };
        assert!(loop_off_the_root.validate().is_err());

        let action_with_child = BehaviourTreeData {
            nodes: vec![
                node(BtNodeKind::Wait { seconds: 1.0 }, vec![1]),
                node(BtNodeKind::Stop, vec![]),
            ],
        };
        assert!(action_with_child.validate().is_err());

        let inverter_with_two = BehaviourTreeData {
            nodes: vec![
                node(BtNodeKind::Inverter, vec![1, 2]),
                node(BtNodeKind::Stop, vec![]),
                node(BtNodeKind::Stop, vec![]),
            ],
        };
        assert!(inverter_with_two.validate().is_err());
    }

    #[test]
    fn bad_settings_are_rejected() {
        let bad = [
            BtNodeKind::Chance { percent: 101.0 },
            BtNodeKind::Wait { seconds: -1.0 },
            BtNodeKind::SendEvent {
                name: String::new(),
            },
            BtNodeKind::MoveTo {
                position: [f32::NAN, 0.0, 0.0],
            },
        ];
        for kind in bad {
            assert!(validate_kind(&kind).is_err(), "{kind:?}");
        }
    }

    #[test]
    fn remove_subtree_drops_descendants_and_renumbers() {
        let mut tree = BehaviourTreeData::default();
        tree.remove_subtree(1);
        assert_eq!(tree.nodes.len(), 2);
        assert_eq!(tree.nodes[0].children, vec![1]);
        assert_eq!(tree.nodes[1].kind, BtNodeKind::Wait { seconds: 1.0 });
        tree.validate().expect("still a tree");
        tree.remove_subtree(0);
        assert_eq!(tree.nodes.len(), 2);
    }

    #[test]
    fn parent_of_finds_the_owner() {
        let tree = BehaviourTreeData::default();
        assert_eq!(tree.parent_of(3), Some(1));
        assert_eq!(tree.parent_of(0), None);
    }
}
