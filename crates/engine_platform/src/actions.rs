//! Action mapping: raw device inputs (keys, mouse buttons) bound to
//! caller-defined, named actions — gameplay code queries "is `Jump`
//! pressed", not "is `Space` pressed", so rebinding or supporting a second
//! device for the same action never touches gameplay code.
//!
//! Deliberately a thin, stateless-per-frame layer over [`InputState`]:
//! [`ActionMap`] only holds the *bindings* (which raw inputs trigger which
//! action); held/pressed/released queries are answered by consulting an
//! [`InputState`] each call, reusing its existing edge-detection rather
//! than duplicating it. No caller-visible per-frame bookkeeping to keep in
//! sync.
//!
//! Keyboard and mouse buttons only — [`crate::PlatformEvent`] doesn't
//! surface gamepad input yet, so there's nothing else to bind to. No
//! analog axes either: every query here is a boolean, matching
//! [`InputState`]'s own held/pressed/released queries.

use std::collections::HashMap;
use std::hash::Hash;

use crate::event::{KeyCode, MouseButton};
use crate::input::InputState;

/// One raw input that can trigger an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    /// A physical keyboard key.
    Key(KeyCode),
    /// A mouse button.
    MouseButton(MouseButton),
}

impl Binding {
    fn is_held(self, input: &InputState) -> bool {
        match self {
            Self::Key(key) => input.is_key_held(key),
            Self::MouseButton(button) => input.is_mouse_button_held(button),
        }
    }

    fn is_pressed(self, input: &InputState) -> bool {
        match self {
            Self::Key(key) => input.is_key_pressed(key),
            Self::MouseButton(button) => input.is_mouse_button_pressed(button),
        }
    }

    fn is_released(self, input: &InputState) -> bool {
        match self {
            Self::Key(key) => input.is_key_released(key),
            Self::MouseButton(button) => input.is_mouse_button_released(button),
        }
    }
}

/// A rebindable table from actions — any `Copy + Eq + Hash` type the
/// caller defines, typically an enum (e.g. `enum PlayerAction { Jump,
/// MoveLeft }`) — to the [`Binding`]s that trigger them.
///
/// Multiple bindings per action are additive: any one of them firing
/// triggers the action (e.g. bind both `W` and `ArrowUp` to the same
/// `MoveUp` action, or both `Space` and left-click to `Attack`).
#[derive(Debug, Clone)]
pub struct ActionMap<A> {
    bindings: HashMap<A, Vec<Binding>>,
}

impl<A> Default for ActionMap<A> {
    /// An empty action map. Implemented by hand rather than derived: a
    /// derived `Default` would incorrectly require `A: Default` too, which
    /// this type never needs (an empty `HashMap` needs nothing from its
    /// key type).
    fn default() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }
}

impl<A: Copy + Eq + Hash> ActionMap<A> {
    /// An empty action map — no actions bound to anything.
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds `binding` to `action`, in addition to any bindings `action`
    /// already has. A `binding` already bound to `action` is not
    /// duplicated. Returns `self` for chaining multiple binds.
    pub fn bind(&mut self, action: A, binding: Binding) -> &mut Self {
        let bindings = self.bindings.entry(action).or_default();
        if !bindings.contains(&binding) {
            bindings.push(binding);
        }
        self
    }

    /// Removes `binding` from `action`, if present.
    ///
    /// Returns whether it was there to remove.
    pub fn unbind(&mut self, action: A, binding: Binding) -> bool {
        match self.bindings.get_mut(&action) {
            Some(bindings) => {
                let before = bindings.len();
                bindings.retain(|&b| b != binding);
                before != bindings.len()
            }
            None => false,
        }
    }

    /// Removes every binding for `action`. `action` still exists as a
    /// valid (now-unbound) key afterward — querying it just always
    /// answers `false`.
    pub fn clear_bindings(&mut self, action: A) {
        self.bindings.remove(&action);
    }

    /// `action`'s currently bound raw inputs, in bind order. Empty if
    /// `action` has never been bound, or was cleared.
    pub fn bindings(&self, action: A) -> &[Binding] {
        self.bindings.get(&action).map_or(&[], Vec::as_slice)
    }

    /// `true` while any of `action`'s bound inputs is held.
    pub fn is_held(&self, action: A, input: &InputState) -> bool {
        self.bindings(action).iter().any(|b| b.is_held(input))
    }

    /// `true` on the tick any of `action`'s bound inputs was just pressed.
    pub fn is_pressed(&self, action: A, input: &InputState) -> bool {
        self.bindings(action).iter().any(|b| b.is_pressed(input))
    }

    /// `true` on the tick any of `action`'s bound inputs was just
    /// released.
    ///
    /// If two different bindings both drive `action` and release on
    /// different ticks (e.g. `W` held, then released while `ArrowUp` is
    /// still held), this fires on each binding's own release — a caller
    /// that wants "the action as a whole is no longer active" should
    /// additionally check `!self.is_held(action, input)`.
    pub fn is_released(&self, action: A, input: &InputState) -> bool {
        self.bindings(action).iter().any(|b| b.is_released(input))
    }
}

impl<A: Copy + Eq + Hash> FromIterator<(A, Binding)> for ActionMap<A> {
    /// Builds an [`ActionMap`] from `(action, binding)` pairs — e.g.
    /// `ActionMap::from_iter([(PlayerAction::Jump, Binding::Key(KeyCode::Space))])`,
    /// or via `.collect()`. Later pairs for the same action add to earlier
    /// ones (same semantics as repeated [`ActionMap::bind`] calls), not
    /// replace them.
    fn from_iter<I: IntoIterator<Item = (A, Binding)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (action, binding) in iter {
            map.bind(action, binding);
        }
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::PlatformEvent;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum TestAction {
        Jump,
        MoveUp,
    }

    #[test]
    fn new_action_map_has_no_bindings() {
        let map: ActionMap<TestAction> = ActionMap::new();
        assert!(map.bindings(TestAction::Jump).is_empty());
    }

    #[test]
    fn unbound_action_queries_are_false_not_panicking() {
        let map: ActionMap<TestAction> = ActionMap::new();
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::Space,
            pressed: true,
            repeat: false,
        });
        assert!(!map.is_held(TestAction::Jump, &input));
        assert!(!map.is_pressed(TestAction::Jump, &input));
        assert!(!map.is_released(TestAction::Jump, &input));
    }

    #[test]
    fn bind_adds_a_binding() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space));
        assert_eq!(
            map.bindings(TestAction::Jump),
            &[Binding::Key(KeyCode::Space)]
        );
    }

    #[test]
    fn bind_same_binding_twice_does_not_duplicate() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space));
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space));
        assert_eq!(map.bindings(TestAction::Jump).len(), 1);
    }

    #[test]
    fn bind_two_different_bindings_to_same_action() {
        let mut map = ActionMap::new();
        map.bind(TestAction::MoveUp, Binding::Key(KeyCode::KeyW));
        map.bind(TestAction::MoveUp, Binding::Key(KeyCode::ArrowUp));
        assert_eq!(map.bindings(TestAction::MoveUp).len(), 2);
    }

    #[test]
    fn bind_returns_self_for_chaining() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space))
            .bind(TestAction::MoveUp, Binding::Key(KeyCode::KeyW));
        assert_eq!(map.bindings(TestAction::Jump).len(), 1);
        assert_eq!(map.bindings(TestAction::MoveUp).len(), 1);
    }

    #[test]
    fn unbind_removes_and_reports_true() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space));
        assert!(map.unbind(TestAction::Jump, Binding::Key(KeyCode::Space)));
        assert!(map.bindings(TestAction::Jump).is_empty());
    }

    #[test]
    fn unbind_missing_binding_reports_false() {
        let mut map: ActionMap<TestAction> = ActionMap::new();
        assert!(!map.unbind(TestAction::Jump, Binding::Key(KeyCode::Space)));
    }

    #[test]
    fn clear_bindings_empties_only_that_action() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space));
        map.bind(TestAction::MoveUp, Binding::Key(KeyCode::KeyW));
        map.clear_bindings(TestAction::Jump);
        assert!(map.bindings(TestAction::Jump).is_empty());
        assert_eq!(map.bindings(TestAction::MoveUp).len(), 1);
    }

    #[test]
    fn is_held_true_via_key_binding() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space));
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::Space,
            pressed: true,
            repeat: false,
        });
        assert!(map.is_held(TestAction::Jump, &input));
    }

    #[test]
    fn is_held_true_via_mouse_button_binding() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::MouseButton(MouseButton::Left));
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::MouseButtonInput {
            button: MouseButton::Left,
            pressed: true,
        });
        assert!(map.is_held(TestAction::Jump, &input));
    }

    #[test]
    fn is_held_false_when_bound_input_is_not_active() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space));
        let input = InputState::new();
        assert!(!map.is_held(TestAction::Jump, &input));
    }

    #[test]
    fn is_held_true_when_either_of_two_bindings_is_held() {
        let mut map = ActionMap::new();
        map.bind(TestAction::MoveUp, Binding::Key(KeyCode::KeyW));
        map.bind(TestAction::MoveUp, Binding::Key(KeyCode::ArrowUp));

        let mut only_w = InputState::new();
        only_w.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::KeyW,
            pressed: true,
            repeat: false,
        });
        assert!(map.is_held(TestAction::MoveUp, &only_w));

        let mut only_arrow = InputState::new();
        only_arrow.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::ArrowUp,
            pressed: true,
            repeat: false,
        });
        assert!(map.is_held(TestAction::MoveUp, &only_arrow));

        let mut both = InputState::new();
        both.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::KeyW,
            pressed: true,
            repeat: false,
        });
        both.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::ArrowUp,
            pressed: true,
            repeat: false,
        });
        assert!(map.is_held(TestAction::MoveUp, &both));
    }

    #[test]
    fn is_pressed_true_only_on_press_tick() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space));
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::Space,
            pressed: true,
            repeat: false,
        });
        assert!(map.is_pressed(TestAction::Jump, &input));

        input.end_frame();
        assert!(!map.is_pressed(TestAction::Jump, &input));
        assert!(map.is_held(TestAction::Jump, &input));
    }

    #[test]
    fn is_released_true_on_release_tick() {
        let mut map = ActionMap::new();
        map.bind(TestAction::Jump, Binding::Key(KeyCode::Space));
        let mut input = InputState::new();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::Space,
            pressed: true,
            repeat: false,
        });
        input.end_frame();
        input.apply_event(&PlatformEvent::KeyboardInput {
            key: KeyCode::Space,
            pressed: false,
            repeat: false,
        });
        assert!(map.is_released(TestAction::Jump, &input));
        assert!(!map.is_held(TestAction::Jump, &input));
    }

    #[test]
    fn from_iter_builds_a_map() {
        let map = ActionMap::from_iter([
            (TestAction::Jump, Binding::Key(KeyCode::Space)),
            (TestAction::MoveUp, Binding::Key(KeyCode::KeyW)),
            (TestAction::MoveUp, Binding::Key(KeyCode::ArrowUp)),
        ]);
        assert_eq!(map.bindings(TestAction::Jump).len(), 1);
        assert_eq!(map.bindings(TestAction::MoveUp).len(), 2);
    }

    #[test]
    fn collect_builds_a_map() {
        let map: ActionMap<TestAction> = [(TestAction::Jump, Binding::Key(KeyCode::Space))]
            .into_iter()
            .collect();
        assert_eq!(map.bindings(TestAction::Jump).len(), 1);
    }
}
