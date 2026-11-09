//! Converts authored scene UI into the engine's retained screen-space tree.

use std::collections::HashMap;

use engine_ui::{Anchor, Color, NodeId, Stack, StackDirection, Style, Ui, Widget};

use crate::{AssetRef, RuleVariables, Scene, UiAnchor, UiElementData, UiKind, UiLayoutDirection};

impl UiElementData {
    fn with_kind(kind: UiKind, text: &str, size: [f32; 2], color: [f32; 4]) -> Self {
        Self {
            kind,
            text: text.into(),
            anchor: UiAnchor::Center,
            offset: [0.0, 0.0],
            size,
            color,
            font_size: engine_ui::DEFAULT_FONT_SIZE,
            corner_radius: 0.0,
            bold: false,
            text_color: [1.0; 4],
            fill_color: [0.35, 0.75, 0.45, 1.0],
            image: None,
            variable: String::new(),
            min: 0.0,
            max: 1.0,
            step: 0.0,
            placeholder: String::new(),
            direction: UiLayoutDirection::Vertical,
            gap: 0.0,
            padding: 0.0,
        }
    }

    /// A visible, editable label with a top-left screen anchor.
    pub fn label() -> Self {
        Self {
            anchor: UiAnchor::TopLeft,
            offset: [24.0, 24.0],
            ..Self::with_kind(UiKind::Label, "Label", [180.0, 32.0], [1.0; 4])
        }
    }

    /// A clickable button with a centered screen anchor.
    pub fn button() -> Self {
        Self {
            corner_radius: 10.0,
            ..Self::with_kind(
                UiKind::Button,
                "Button",
                [180.0, 44.0],
                [0.33, 0.48, 0.95, 1.0],
            )
        }
    }

    /// A dark rectangle to put other elements on.
    pub fn panel() -> Self {
        Self {
            corner_radius: 16.0,
            ..Self::with_kind(UiKind::Panel, "", [320.0, 240.0], [0.09, 0.1, 0.15, 0.94])
        }
    }

    /// A picture; choose its file in the Inspector.
    pub fn image() -> Self {
        Self::with_kind(UiKind::Image, "", [128.0, 128.0], [1.0; 4])
    }

    /// A bar showing the `health` variable out of 100.
    pub fn progress_bar() -> Self {
        Self {
            variable: "health".into(),
            max: 100.0,
            corner_radius: 10.0,
            ..Self::with_kind(
                UiKind::ProgressBar,
                "",
                [240.0, 20.0],
                [0.2, 0.22, 0.3, 1.0],
            )
        }
    }

    /// A slider changing the `volume` variable from 0 to 1.
    pub fn slider() -> Self {
        Self {
            variable: "volume".into(),
            ..Self::with_kind(UiKind::Slider, "", [240.0, 24.0], [0.2, 0.22, 0.3, 1.0])
        }
    }

    /// An on/off box changing the `sound_on` variable.
    pub fn toggle() -> Self {
        Self {
            variable: "sound_on".into(),
            corner_radius: 6.0,
            ..Self::with_kind(
                UiKind::Toggle,
                "Toggle",
                [200.0, 24.0],
                [0.2, 0.22, 0.3, 1.0],
            )
        }
    }

    /// An empty field the player types into.
    pub fn text_input() -> Self {
        Self {
            placeholder: "Type here".into(),
            corner_radius: 8.0,
            ..Self::with_kind(UiKind::TextInput, "", [240.0, 36.0], [0.2, 0.22, 0.3, 1.0])
        }
    }

    /// A clear box that lists its children from top to bottom.
    pub fn layout() -> Self {
        Self {
            gap: 8.0,
            ..Self::with_kind(UiKind::Layout, "", [240.0, 240.0], [0.0; 4])
        }
    }

    /// The starting values for a new element of `kind`.
    pub fn for_kind(kind: UiKind) -> Self {
        match kind {
            UiKind::Label => Self::label(),
            UiKind::Button => Self::button(),
            UiKind::Panel => Self::panel(),
            UiKind::Image => Self::image(),
            UiKind::ProgressBar => Self::progress_bar(),
            UiKind::Slider => Self::slider(),
            UiKind::Toggle => Self::toggle(),
            UiKind::TextInput => Self::text_input(),
            UiKind::Layout => Self::layout(),
        }
    }

    fn widget(&self) -> Widget {
        let color = linear(self.color);
        let text_color = linear(self.text_color);
        let fill = linear(self.fill_color);
        match self.kind {
            UiKind::Label => Widget::Label {
                text: self.text.clone(),
                color,
            },
            UiKind::Button => Widget::Button {
                text: self.text.clone(),
                bg: color,
                hot_bg: Color {
                    r: (color.r + 0.12).min(1.0),
                    g: (color.g + 0.12).min(1.0),
                    b: (color.b + 0.12).min(1.0),
                    a: color.a,
                },
            },
            UiKind::Panel | UiKind::Layout => Widget::Panel { color },
            UiKind::Image => match self.image.as_ref().and_then(|image| image.parse_id().ok()) {
                Some(id) => Widget::Image {
                    texture: engine_utils::AssetId::from_uuid(id),
                    tint: color,
                },
                None => Widget::Panel { color },
            },
            UiKind::ProgressBar => Widget::ProgressBar {
                value: self.min,
                min: self.min,
                max: self.max,
                track: color,
                fill,
            },
            UiKind::Slider => Widget::Slider {
                value: self.min,
                min: self.min,
                max: self.max,
                step: self.step,
                track: color,
                fill,
            },
            UiKind::Toggle => Widget::Toggle {
                text: self.text.clone(),
                on: false,
                off_color: color,
                on_color: fill,
            },
            UiKind::TextInput => Widget::TextInput {
                text: self.text.clone(),
                placeholder: self.placeholder.clone(),
                bg: color,
                color: text_color,
            },
        }
    }
}

fn linear(color: [f32; 4]) -> Color {
    Color {
        r: color[0],
        g: color[1],
        b: color[2],
        a: color[3],
    }
}

impl From<UiAnchor> for Anchor {
    fn from(anchor: UiAnchor) -> Self {
        match anchor {
            UiAnchor::TopLeft => Self::TopLeft,
            UiAnchor::Top => Self::Top,
            UiAnchor::TopRight => Self::TopRight,
            UiAnchor::Left => Self::Left,
            UiAnchor::Center => Self::Center,
            UiAnchor::Right => Self::Right,
            UiAnchor::BottomLeft => Self::BottomLeft,
            UiAnchor::Bottom => Self::Bottom,
            UiAnchor::BottomRight => Self::BottomRight,
        }
    }
}

/// One element for [`mount_ui_elements`].
#[derive(Debug, Clone, Copy)]
pub struct UiMountItem<'a> {
    /// The element.
    pub data: &'a UiElementData,
    /// Index in the list of its nearest UI ancestor. `None` places it on the screen.
    pub parent: Option<usize>,
    /// Whether it starts hidden.
    pub hidden: bool,
}

/// Builds `items` into `ui` and returns each item's node, in the same order.
/// Clears `ui` first. Siblings paint and list in the given order.
pub fn mount_ui_elements(ui: &mut Ui, items: &[UiMountItem<'_>]) -> Vec<NodeId> {
    ui.clear();
    append_ui_elements(ui, items)
}

/// [`mount_ui_elements`] without clearing what is already there.
fn append_ui_elements(ui: &mut Ui, items: &[UiMountItem<'_>]) -> Vec<NodeId> {
    let mut nodes = vec![None; items.len()];
    for index in 0..items.len() {
        mount_item(ui, items, &mut nodes, index, items.len());
    }
    nodes.into_iter().flatten().collect()
}

/// Mounts `index` after its ancestors, which may come later in the list.
/// `depth` stops a parent cycle; such an element falls back to the screen.
fn mount_item(
    ui: &mut Ui,
    items: &[UiMountItem<'_>],
    nodes: &mut [Option<NodeId>],
    index: usize,
    depth: usize,
) -> NodeId {
    if let Some(node) = nodes[index] {
        return node;
    }
    let item = items[index];
    let parent = item
        .parent
        .filter(|parent| *parent < items.len() && depth > 0)
        .map(|parent| mount_item(ui, items, nodes, parent, depth - 1));
    // The cycle's last call may have mounted this one already.
    if let Some(node) = nodes[index] {
        return node;
    }
    let style = Style {
        anchor: item.data.anchor.into(),
        offset: item.data.offset,
        size: item.data.size,
    };
    let widget = item.data.widget();
    let node = match parent {
        Some(parent) => ui.add(Some(parent), style, widget),
        None => ui.add_screen(style, widget),
    };
    if let Some(mounted) = ui.node_mut(node) {
        mounted.font_size = item.data.font_size;
        mounted.corner_radius = item.data.corner_radius;
        mounted.bold = item.data.bold;
        mounted.text_color = Some(linear(item.data.text_color));
        if item.data.kind == UiKind::Layout {
            mounted.stack = Some(Stack {
                direction: match item.data.direction {
                    UiLayoutDirection::Vertical => StackDirection::Vertical,
                    UiLayoutDirection::Horizontal => StackDirection::Horizontal,
                },
                gap: item.data.gap,
                padding: item.data.padding,
            });
        }
        if item.data.kind.uses_variable() && !item.data.variable.is_empty() {
            mounted.variable = Some(item.data.variable.clone());
        }
    }
    if item.hidden {
        ui.set_visible(node, false);
    }
    nodes[index] = Some(node);
    node
}

impl Scene {
    /// Rebuilds authored UI for this scene. Node IDs map to stable scene object
    /// IDs for gameplay commands. Disabled elements are mounted hidden, so
    /// showing or enabling them later works. An element whose parent is also a
    /// UI element sits inside it; other parents do not affect its position.
    pub fn mount_ui(&self, ui: &mut Ui) -> HashMap<NodeId, String> {
        ui.clear();
        self.append_ui(ui)
    }

    /// [`Scene::mount_ui`] without clearing what is already there, so this
    /// scene's elements go on top: for a scene loaded on top of another, or a
    /// loading screen.
    pub fn append_ui(&self, ui: &mut Ui) -> HashMap<NodeId, String> {
        let elements: Vec<usize> = self
            .entities
            .iter()
            .enumerate()
            .filter_map(|(index, entity)| entity.ui.is_some().then_some(index))
            .collect();
        let item_of: HashMap<usize, usize> = elements
            .iter()
            .enumerate()
            .map(|(item, entity)| (*entity, item))
            .collect();
        let items: Vec<UiMountItem<'_>> = elements
            .iter()
            .filter_map(|&index| {
                let entity = &self.entities[index];
                // Bounded so a bad parent chain cannot hang the walk.
                let parent = std::iter::successors(entity.parent, |&ancestor| {
                    self.entities.get(ancestor).and_then(|up| up.parent)
                })
                .take(self.entities.len())
                .find_map(|ancestor| item_of.get(&ancestor).copied());
                Some(UiMountItem {
                    data: entity.ui.as_ref()?,
                    parent,
                    hidden: entity.disabled,
                })
            })
            .collect();
        let mounted = append_ui_elements(ui, &items);
        let mut nodes = HashMap::new();
        for (node, index) in mounted.into_iter().zip(elements) {
            if let Some(id) = &self.entities[index].object_id {
                nodes.insert(node, id.clone());
            }
        }
        nodes
    }

    /// Each different picture this scene's image elements show.
    pub fn ui_image_refs(&self) -> Vec<AssetRef> {
        let mut images: Vec<AssetRef> = Vec::new();
        for image in self
            .entities
            .iter()
            .filter_map(|entity| entity.ui.as_ref()?.image.as_ref())
        {
            if !images.contains(image) {
                images.push(image.clone());
            }
        }
        images
    }
}

fn authored_node(nodes: &HashMap<NodeId, String>, object_id: &str) -> Option<NodeId> {
    nodes
        .iter()
        .find_map(|(node, id)| (id == object_id).then_some(*node))
}

/// Changes a mounted scene UI caption, or a text field's text, by stable object ID.
pub fn set_authored_ui_text(
    ui: &mut Ui,
    nodes: &HashMap<NodeId, String>,
    object_id: &str,
    text: &str,
) -> bool {
    let node = authored_node(nodes, object_id);
    match node.and_then(|node| ui.node_mut(node)) {
        Some(engine_ui::Node {
            widget:
                Widget::Label { text: caption, .. }
                | Widget::Button { text: caption, .. }
                | Widget::Toggle { text: caption, .. }
                | Widget::TextInput { text: caption, .. },
            ..
        }) => {
            *caption = text.to_owned();
            true
        }
        _ => false,
    }
}

/// The current text of a mounted UI element: what the player typed into a text
/// field, or a label, button or toggle's caption. `None` if it has no text.
pub fn authored_ui_text<'a>(
    ui: &'a Ui,
    nodes: &HashMap<NodeId, String>,
    object_id: &str,
) -> Option<&'a str> {
    match &ui.node(authored_node(nodes, object_id)?)?.widget {
        Widget::Label { text, .. }
        | Widget::Button { text, .. }
        | Widget::Toggle { text, .. }
        | Widget::TextInput { text, .. } => Some(text),
        _ => None,
    }
}

/// Makes every bar, slider and toggle bound to a variable show its value.
pub fn sync_ui_variables(ui: &mut Ui, variables: &RuleVariables) {
    for id in 0..ui.node_count() {
        let value = ui
            .node(id)
            .and_then(|node| node.variable.as_deref())
            .map(|name| variables.get(name));
        if let Some(value) = value {
            ui.set_value(id, value as f32);
        }
    }
}

#[cfg(test)]
mod element_tests {
    use super::*;
    use crate::SceneEntity;

    fn element(id: &str, data: UiElementData, parent: Option<usize>) -> SceneEntity {
        SceneEntity {
            object_id: Some(id.into()),
            ui: Some(data),
            parent,
            ..Default::default()
        }
    }

    fn node_of(nodes: &HashMap<NodeId, String>, id: &str) -> NodeId {
        authored_node(nodes, id).expect("mounted")
    }

    #[test]
    fn old_scene_text_reads_with_defaults_and_saves_without_new_fields() {
        let text = r#"(kind: Label, text: "Hi", anchor: Top, offset: (1.0, 2.0), size: (10.0, 20.0), color: (1.0, 1.0, 1.0, 1.0))"#;
        let data: UiElementData = ron::from_str(text).expect("old element reads");
        assert_eq!(data.font_size, engine_ui::DEFAULT_FONT_SIZE);
        assert_eq!(data.max, 1.0);
        data.validate().expect("valid");
        let saved = ron::to_string(&data).expect("saves");
        for new_field in ["font_size", "text_color", "fill_color", "variable", "gap"] {
            assert!(!saved.contains(new_field), "{new_field} written: {saved}");
        }
    }

    #[test]
    fn every_new_kind_round_trips_and_validates() {
        for data in [
            UiElementData::panel(),
            UiElementData::image(),
            UiElementData::progress_bar(),
            UiElementData::slider(),
            UiElementData::toggle(),
            UiElementData::text_input(),
            UiElementData::layout(),
        ] {
            data.validate().expect("default is valid");
            let text = ron::to_string(&data).expect("saves");
            let back: UiElementData = ron::from_str(&text).expect("reads");
            assert_eq!(back, data);
        }
    }

    #[test]
    fn bad_values_are_refused() {
        let broken: [fn(&mut UiElementData); 6] = [
            |data| data.font_size = 1.0,
            |data| data.max = data.min,
            |data| data.gap = -1.0,
            |data| data.step = f32::NAN,
            |data| data.fill_color[0] = 2.0,
            |data| {
                data.image = Some(AssetRef {
                    id: "not-a-uuid".into(),
                })
            },
        ];
        for breakage in broken {
            let mut data = UiElementData::slider();
            breakage(&mut data);
            assert!(data.validate().is_err(), "{data:?}");
        }
    }

    #[test]
    fn a_child_sits_inside_its_ui_parent_and_a_layout_lists_children() {
        let scene = Scene {
            entities: vec![
                // A child listed before its parent still ends up inside it.
                element(
                    "second",
                    UiElementData {
                        size: [50.0, 20.0],
                        offset: [99.0, 99.0],
                        anchor: UiAnchor::TopLeft,
                        ..UiElementData::label()
                    },
                    Some(3),
                ),
                element(
                    "first",
                    UiElementData {
                        size: [50.0, 30.0],
                        anchor: UiAnchor::TopLeft,
                        ..UiElementData::button()
                    },
                    Some(3),
                ),
                SceneEntity {
                    ui: None,
                    ..Default::default()
                },
                element(
                    "list",
                    UiElementData {
                        anchor: UiAnchor::TopLeft,
                        offset: [10.0, 10.0],
                        gap: 5.0,
                        padding: 1.0,
                        ..UiElementData::layout()
                    },
                    None,
                ),
                // Parented to a non-UI object: stays on the screen.
                element(
                    "free",
                    UiElementData {
                        anchor: UiAnchor::TopLeft,
                        offset: [7.0, 7.0],
                        ..UiElementData::label()
                    },
                    Some(2),
                ),
            ],
            ..Default::default()
        };
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        ui.layout();
        let rect = |id: &str| ui.rect_of(node_of(&nodes, id)).expect("rect");
        assert_eq!((rect("second").x, rect("second").y), (11.0, 11.0));
        assert_eq!(rect("first").y, 11.0 + 20.0 + 5.0);
        assert_eq!((rect("free").x, rect("free").y), (7.0, 7.0));
    }

    #[test]
    fn variables_drive_bars_sliders_and_toggles() {
        let scene = Scene {
            entities: vec![
                element("bar", UiElementData::progress_bar(), None),
                element("slider", UiElementData::slider(), None),
                element("toggle", UiElementData::toggle(), None),
            ],
            ..Default::default()
        };
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        let mut variables = RuleVariables::default();
        variables.set("health", 40.0).expect("set");
        variables.set("volume", 0.5).expect("set");
        variables.set("sound_on", 1.0).expect("set");
        sync_ui_variables(&mut ui, &variables);
        assert!(matches!(
            ui.node(node_of(&nodes, "bar")).expect("bar").widget,
            Widget::ProgressBar { value: 40.0, .. }
        ));
        assert!(matches!(
            ui.node(node_of(&nodes, "slider")).expect("slider").widget,
            Widget::Slider { value: 0.5, .. }
        ));
        assert!(matches!(
            ui.node(node_of(&nodes, "toggle")).expect("toggle").widget,
            Widget::Toggle { on: true, .. }
        ));
        assert!(ui.take_changes().is_empty(), "not the player's change");
    }

    #[test]
    fn text_fields_read_and_write_by_object_id() {
        let scene = Scene {
            entities: vec![
                element("name", UiElementData::text_input(), None),
                element("box", UiElementData::panel(), None),
            ],
            ..Default::default()
        };
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        assert_eq!(authored_ui_text(&ui, &nodes, "name"), Some(""));
        assert!(set_authored_ui_text(&mut ui, &nodes, "name", "Ana"));
        assert_eq!(authored_ui_text(&ui, &nodes, "name"), Some("Ana"));
        assert!(!set_authored_ui_text(&mut ui, &nodes, "box", "x"));
        assert_eq!(authored_ui_text(&ui, &nodes, "box"), None);
    }

    #[test]
    fn a_click_rule_may_sit_on_a_text_field_but_not_on_a_panel() {
        let rule = crate::RuleData {
            trigger: crate::RuleTrigger::UiClicked,
            action: crate::RuleAction::EmitEvent {
                name: "done".into(),
            },
            conditions: Vec::new(),
            match_mode: crate::RuleMatch::All,
            graph_position: None,
        };
        let with = |data| Scene {
            entities: vec![SceneEntity {
                ui: Some(data),
                rules: vec![rule.clone()],
                ..Default::default()
            }],
            ..Default::default()
        };
        with(UiElementData::text_input()).validate().expect("valid");
        assert!(with(UiElementData::panel()).validate().is_err());
    }

    #[test]
    fn image_refs_are_listed_once() {
        let image = AssetRef {
            id: "3fa85f64-5717-4562-b3fc-2c963f66afa6".into(),
        };
        let picture = || UiElementData {
            image: Some(image.clone()),
            ..UiElementData::image()
        };
        let scene = Scene {
            entities: vec![
                element("a", picture(), None),
                element("b", picture(), None),
                element("c", UiElementData::image(), None),
            ],
            ..Default::default()
        };
        assert_eq!(scene.ui_image_refs(), vec![image]);
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        assert!(matches!(
            ui.node(node_of(&nodes, "a")).expect("a").widget,
            Widget::Image { .. }
        ));
        assert!(
            matches!(
                ui.node(node_of(&nodes, "c")).expect("c").widget,
                Widget::Panel { .. }
            ),
            "no picture chosen yet"
        );
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;
    use crate::{RuleAction, RuleData, RuleEvent, RuleTrigger, SceneEntity};
    use engine_ecs::prelude::World;

    #[test]
    fn a_button_rule_can_emit_an_event_that_runs_followup_actions() {
        let button_id = "67d09e47-5520-4b5a-a132-a68a4a60f452";
        let label_id = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
        let scene = Scene {
            entities: vec![
                SceneEntity {
                    object_id: Some(button_id.into()),
                    ui: Some(UiElementData::button()),
                    rules: vec![
                        RuleData {
                            trigger: RuleTrigger::UiClicked,
                            action: RuleAction::SetUiText {
                                target: label_id.into(),
                                text: "First".into(),
                            },
                            conditions: Vec::new(),
                            match_mode: crate::RuleMatch::All,
                            graph_position: None,
                        },
                        RuleData {
                            trigger: RuleTrigger::UiClicked,
                            action: RuleAction::EmitEvent {
                                name: "stage_two".into(),
                            },
                            conditions: Vec::new(),
                            match_mode: crate::RuleMatch::All,
                            graph_position: None,
                        },
                    ],
                    ..Default::default()
                },
                SceneEntity {
                    object_id: Some(label_id.into()),
                    ui: Some(UiElementData::label()),
                    rules: vec![RuleData {
                        trigger: RuleTrigger::Named("stage_two".into()),
                        action: RuleAction::SetUiText {
                            target: label_id.into(),
                            text: "Second".into(),
                        },
                        conditions: Vec::new(),
                        match_mode: crate::RuleMatch::All,
                        graph_position: None,
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        scene.validate().expect("valid event chain");
        let scene = Scene::from_ron_str(&scene.to_ron_string().expect("serialize chain"))
            .expect("reopen event chain");
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        let mut world = World::new();
        scene.instantiate(&mut world);
        assert_eq!(
            scene
                .apply_rule_event(RuleEvent::UiClicked(button_id), &mut world, &mut ui, &nodes)
                .applied,
            3
        );
        let label_node = nodes
            .iter()
            .find_map(|(node, id)| (id == label_id).then_some(*node))
            .expect("label node");
        assert!(matches!(
            &ui.node(label_node).expect("label").widget,
            Widget::Label { text, .. } if text == "Second"
        ));
    }

    #[test]
    fn a_visual_event_cycle_stops_at_a_bounded_number_of_actions() {
        let scene = Scene {
            entities: vec![SceneEntity {
                rules: vec![RuleData {
                    trigger: RuleTrigger::Named("again".into()),
                    action: RuleAction::EmitEvent {
                        name: "again".into(),
                    },
                    conditions: Vec::new(),
                    match_mode: crate::RuleMatch::All,
                    graph_position: None,
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        scene.validate().expect("valid cyclic graph");
        let mut ui = Ui::new(800.0, 600.0);
        let mut world = World::new();
        let count = scene
            .apply_rule_event(
                RuleEvent::Named("again"),
                &mut world,
                &mut ui,
                &HashMap::new(),
            )
            .applied;
        assert!((1..=64).contains(&count), "a cycle must terminate promptly");
    }

    #[test]
    fn branching_event_chains_bound_total_actions_not_only_event_count() {
        let label_id = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
        let mut scene = Scene::default();
        scene.entities.push(SceneEntity {
            object_id: Some(label_id.into()),
            ui: Some(UiElementData::label()),
            ..Default::default()
        });
        for _ in 0..4 {
            let mut rules = Vec::new();
            for _ in 0..8 {
                rules.push(RuleData {
                    trigger: RuleTrigger::Named("loop".into()),
                    action: RuleAction::SetUiText {
                        target: label_id.into(),
                        text: "Still running".into(),
                    },
                    conditions: Vec::new(),
                    match_mode: crate::RuleMatch::All,
                    graph_position: None,
                });
                rules.push(RuleData {
                    trigger: RuleTrigger::Named("loop".into()),
                    action: RuleAction::EmitEvent {
                        name: "loop".into(),
                    },
                    conditions: Vec::new(),
                    match_mode: crate::RuleMatch::All,
                    graph_position: None,
                });
            }
            scene.entities.push(SceneEntity {
                rules,
                ..Default::default()
            });
        }
        scene.validate().expect("valid branching graph");
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        let mut world = World::new();
        let count = scene
            .apply_rule_event(RuleEvent::Named("loop"), &mut world, &mut ui, &nodes)
            .applied;
        assert!((1..=1024).contains(&count), "actions must stay bounded");
    }

    #[test]
    fn clicking_a_button_runs_its_text_rule_once() {
        let button_id = "67d09e47-5520-4b5a-a132-a68a4a60f452";
        let label_id = "3fa85f64-5717-4562-b3fc-2c963f66afa6";
        let scene = Scene {
            entities: vec![
                SceneEntity {
                    object_id: Some(button_id.into()),
                    ui: Some(UiElementData::button()),
                    rules: vec![RuleData {
                        trigger: RuleTrigger::UiClicked,
                        action: RuleAction::SetUiText {
                            target: label_id.into(),
                            text: "Ready".into(),
                        },
                        conditions: Vec::new(),
                        match_mode: crate::RuleMatch::All,
                        graph_position: None,
                    }],
                    ..Default::default()
                },
                SceneEntity {
                    object_id: Some(label_id.into()),
                    ui: Some(UiElementData::label()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = scene.mount_ui(&mut ui);
        let mut world = World::new();
        scene.instantiate(&mut world);
        assert_eq!(
            scene
                .apply_rule_event(RuleEvent::UiClicked(button_id), &mut world, &mut ui, &nodes)
                .applied,
            1
        );
        let label_node = nodes
            .iter()
            .find_map(|(node, id)| (id == label_id).then_some(*node))
            .unwrap();
        assert!(matches!(
            &ui.node(label_node).unwrap().widget,
            Widget::Label { text, .. } if text == "Ready"
        ));
        assert_eq!(
            scene
                .apply_rule_event(RuleEvent::UiClicked(label_id), &mut world, &mut ui, &nodes)
                .applied,
            0
        );
    }

    #[test]
    fn named_event_runs_audio_action_and_skips_missing_target() {
        let audio_id = "72ec89b0-b7e5-45b1-87d9-71bd110a3f47";
        let missing_id = "52265ff6-dafa-44a3-8d90-6a4d09e13d4d";
        let scene = Scene {
            entities: vec![SceneEntity {
                rules: vec![
                    RuleData {
                        trigger: RuleTrigger::Named("game_start".into()),
                        action: RuleAction::PlayAudio {
                            target: audio_id.into(),
                        },
                        conditions: Vec::new(),
                        match_mode: crate::RuleMatch::All,
                        graph_position: None,
                    },
                    RuleData {
                        trigger: RuleTrigger::Named("game_start".into()),
                        action: RuleAction::PlayAudio {
                            target: missing_id.into(),
                        },
                        conditions: Vec::new(),
                        match_mode: crate::RuleMatch::All,
                        graph_position: None,
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let wav = Vec::from(&b"RIFF\x26\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\x02\0\0\0\0\0"[..]);
        let sound = engine_audio::StaticSound::from_bytes(wav, false).unwrap();
        let mut world = World::new();
        world.spawn((
            crate::SceneObjectId(audio_id.into()),
            engine_ecs::AudioEmitter::silent(sound),
        ));
        let mut ui = Ui::new(800.0, 600.0);
        let nodes = HashMap::new();

        assert_eq!(
            scene
                .apply_rule_event(RuleEvent::Named("other"), &mut world, &mut ui, &nodes)
                .applied,
            0
        );
        assert_eq!(
            scene
                .apply_rule_event(RuleEvent::Named("game_start"), &mut world, &mut ui, &nodes)
                .applied,
            1
        );
    }
}
