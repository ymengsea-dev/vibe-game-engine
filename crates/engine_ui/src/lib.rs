//! # engine_ui
//!
//! Runtime game UI: a retained node tree with anchor-based layout,
//! pointer hit-testing, click handling, and a flat draw list a renderer
//! turns into quads. Deliberately **not** egui — the editor's egui stack
//! must never end up in a shipped game (NFR-004), so this is a small,
//! dependency-free model the `engine` facade re-exports for game code.
//!
//! ## Shape
//!
//! A [`Ui`] owns every [`Node`]; the game builds/rebuilds the tree each
//! frame (or keeps it and mutates it), calls [`Ui::layout`] once the
//! screen size is known, feeds pointer state to [`Ui::interact`] to get
//! back the [`NodeId`]s that were clicked, and hands [`Ui::draw_list`]
//! to the renderer.
//!
//! ## Not done here
//!
//! - No glyph rendering — a [`Widget::Label`]/[`Widget::Button`] emits a
//!   [`DrawKind::Text`] command carrying the string; wiring that to a
//!   font atlas in `engine_renderer` is the next slice.
//! - No scrolling, no flex/grid, no focus/keyboard nav — anchor +
//!   offset + fixed size only.

use engine_utils::AssetId;

/// Index of a [`Node`] within its owning [`Ui`]. Stable for the lifetime
/// of the tree; invalidated by [`Ui::clear`].
pub type NodeId = usize;

/// Linear RGBA colour, components in `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha (`0` transparent, `1` opaque).
    pub a: f32,
}

impl Color {
    /// Fully transparent.
    pub const TRANSPARENT: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    /// Opaque white.
    pub const WHITE: Color = Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    /// An opaque colour from linear RGB.
    pub const fn rgb(r: f32, g: f32, b: f32) -> Color {
        Color { r, g, b, a: 1.0 }
    }
}

/// An axis-aligned rectangle in screen pixels, origin top-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// Whether `point` (`[x, y]`) lies inside this rectangle (inclusive
    /// of the top-left edge, exclusive of the bottom-right).
    pub fn contains(&self, point: [f32; 2]) -> bool {
        point[0] >= self.x
            && point[0] < self.x + self.w
            && point[1] >= self.y
            && point[1] < self.y + self.h
    }
}

/// Where a node's box attaches within its parent's rectangle.
/// [`Style::offset`] is then added in pixels (positive = right / down).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Anchor {
    /// Parent's top-left corner.
    #[default]
    TopLeft,
    /// Centred on the parent's top edge.
    Top,
    /// Parent's top-right corner.
    TopRight,
    /// Centred on the parent's left edge.
    Left,
    /// Parent's centre.
    Center,
    /// Centred on the parent's right edge.
    Right,
    /// Parent's bottom-left corner.
    BottomLeft,
    /// Centred on the parent's bottom edge.
    Bottom,
    /// Parent's bottom-right corner.
    BottomRight,
}

impl Anchor {
    /// The anchor's fractional position within a unit box:
    /// `(0,0)` = top-left, `(1,1)` = bottom-right.
    fn factors(self) -> (f32, f32) {
        match self {
            Anchor::TopLeft => (0.0, 0.0),
            Anchor::Top => (0.5, 0.0),
            Anchor::TopRight => (1.0, 0.0),
            Anchor::Left => (0.0, 0.5),
            Anchor::Center => (0.5, 0.5),
            Anchor::Right => (1.0, 0.5),
            Anchor::BottomLeft => (0.0, 1.0),
            Anchor::Bottom => (0.5, 1.0),
            Anchor::BottomRight => (1.0, 1.0),
        }
    }
}

/// Layout for one node: where it anchors in its parent, a pixel offset
/// from that anchor, and its pixel size. The node's own box uses the
/// same anchor as its alignment point (so an `Anchor::Center` node is
/// centred on the parent's centre).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Attachment point within the parent.
    pub anchor: Anchor,
    /// Pixels added to the anchor point (`[dx, dy]`).
    pub offset: [f32; 2],
    /// Node size in pixels (`[w, h]`).
    pub size: [f32; 2],
}

impl Default for Style {
    fn default() -> Self {
        Self {
            anchor: Anchor::TopLeft,
            offset: [0.0, 0.0],
            size: [0.0, 0.0],
        }
    }
}

/// What a node draws.
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    /// A solid rectangle.
    Panel {
        /// Fill colour.
        color: Color,
    },
    /// A text run (no glyph rendering yet — see the module docs).
    Label {
        /// The string.
        text: String,
        /// Text colour.
        color: Color,
    },
    /// A clickable rectangle with a label. `interact` reports it in the
    /// clicked list on a press-then-release inside its box.
    Button {
        /// Button caption.
        text: String,
        /// Background when idle.
        bg: Color,
        /// Background while the pointer is over it.
        hot_bg: Color,
    },
    /// A textured rectangle.
    Image {
        /// The texture asset to sample.
        texture: AssetId,
        /// Multiplied into the sampled colour.
        tint: Color,
    },
}

/// One node in the [`Ui`] tree.
#[derive(Debug, Clone)]
pub struct Node {
    /// Layout inputs.
    pub style: Style,
    /// What it draws / how it behaves.
    pub widget: Widget,
    /// Child node ids, painted after (on top of) this node.
    children: Vec<NodeId>,
    /// Filled by [`Ui::layout`]; meaningless before the first call.
    computed: Rect,
}

/// A retained UI tree.
#[derive(Debug, Default)]
pub struct Ui {
    nodes: Vec<Node>,
    root: Option<NodeId>,
    screen: [f32; 2],
    /// The node a pointer press started over, tracked so a release only
    /// counts as a click if it lands on the same node.
    press_target: Option<NodeId>,
}

/// The pointer state for one [`Ui::interact`] call.
#[derive(Debug, Clone, Copy, Default)]
pub struct PointerInput {
    /// Cursor position in screen pixels.
    pub position: [f32; 2],
    /// `true` on the frame the primary button went down.
    pub pressed: bool,
    /// `true` on the frame the primary button went up.
    pub released: bool,
}

/// One entry in [`Ui::draw_list`], in back-to-front paint order.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawCommand {
    /// Screen-space rectangle to draw into.
    pub rect: Rect,
    /// What to draw.
    pub kind: DrawKind,
}

/// The payload of a [`DrawCommand`].
#[derive(Debug, Clone, PartialEq)]
pub enum DrawKind {
    /// Fill the rect with a solid colour.
    Fill(Color),
    /// Draw `texture` into the rect, multiplied by `tint`.
    Image {
        /// Texture asset.
        texture: AssetId,
        /// Tint colour.
        tint: Color,
    },
    /// Draw `text` in `color`, left-aligned within the rect. The
    /// renderer owns font/size for now.
    Text {
        /// The string.
        text: String,
        /// Text colour.
        color: Color,
    },
}

impl Ui {
    /// An empty tree sized for a `width` x `height` screen.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            nodes: Vec::new(),
            root: None,
            screen: [width.max(0.0), height.max(0.0)],
            press_target: None,
        }
    }

    /// Updates the screen size used as the root node's parent rect.
    pub fn set_screen(&mut self, width: f32, height: f32) {
        self.screen = [width.max(0.0), height.max(0.0)];
    }

    /// Removes every node. Existing [`NodeId`]s become invalid.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.root = None;
        self.press_target = None;
    }

    /// Adds a node under `parent` (or as the root when `parent` is
    /// `None` and there is no root yet) and returns its id.
    ///
    /// A second `None` parent after a root exists attaches to the root.
    /// An out-of-range `parent` also attaches to the root (or becomes
    /// the root if there isn't one) rather than panicking.
    pub fn add(&mut self, parent: Option<NodeId>, style: Style, widget: Widget) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node {
            style,
            widget,
            children: Vec::new(),
            computed: Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
        });

        let resolved_parent = match parent {
            Some(p) if p < id => Some(p),
            _ => self.root,
        };
        match resolved_parent {
            Some(p) => self.nodes[p].children.push(id),
            None => self.root = Some(id),
        }
        id
    }

    /// This node's laid-out rectangle (valid after [`Ui::layout`]).
    pub fn rect_of(&self, id: NodeId) -> Option<Rect> {
        self.nodes.get(id).map(|node| node.computed)
    }

    /// Read access to a node.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Mutable access to a node's style/widget (call [`Ui::layout`]
    /// again after changing a style).
    pub fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(id)
    }

    /// Computes every node's screen rectangle, top-down from the root.
    /// Cheap enough to call every frame.
    pub fn layout(&mut self) {
        let Some(root) = self.root else { return };
        let screen = Rect {
            x: 0.0,
            y: 0.0,
            w: self.screen[0],
            h: self.screen[1],
        };
        self.layout_node(root, screen);
    }

    fn layout_node(&mut self, id: NodeId, parent: Rect) {
        let (style, children) = {
            let node = &self.nodes[id];
            (node.style, node.children.clone())
        };
        let (fx, fy) = style.anchor.factors();
        let anchor_x = parent.x + parent.w * fx + style.offset[0];
        let anchor_y = parent.y + parent.h * fy + style.offset[1];
        let rect = Rect {
            x: anchor_x - style.size[0] * fx,
            y: anchor_y - style.size[1] * fy,
            w: style.size[0],
            h: style.size[1],
        };
        self.nodes[id].computed = rect;
        for child in children {
            self.layout_node(child, rect);
        }
    }

    /// The topmost node whose laid-out rect contains `point`, or `None`.
    /// "Topmost" = last painted: a later sibling, and a child over its
    /// parent.
    pub fn hit(&self, point: [f32; 2]) -> Option<NodeId> {
        let root = self.root?;
        self.hit_node(root, point)
    }

    fn hit_node(&self, id: NodeId, point: [f32; 2]) -> Option<NodeId> {
        let node = &self.nodes[id];
        // Children are on top; test them back-to-front.
        for &child in node.children.iter().rev() {
            if let Some(hit) = self.hit_node(child, point) {
                return Some(hit);
            }
        }
        if node.computed.contains(point) {
            Some(id)
        } else {
            None
        }
    }

    /// Feeds one frame of pointer input and returns the ids of every
    /// [`Widget::Button`] that completed a click this frame (pressed and
    /// released over the same button).
    pub fn interact(&mut self, input: PointerInput) -> Vec<NodeId> {
        let over = self.hit(input.position);

        if input.pressed {
            self.press_target = over;
        }

        let mut clicked = Vec::new();
        if input.released {
            if let (Some(pressed), Some(released)) = (self.press_target, over)
                && pressed == released
                && matches!(self.nodes[released].widget, Widget::Button { .. })
            {
                clicked.push(released);
            }
            self.press_target = None;
        }
        clicked
    }

    /// Whether the pointer at `position` is over `id` — the hook a
    /// caller uses to pick a button's hot colour.
    pub fn is_hot(&self, id: NodeId, position: [f32; 2]) -> bool {
        self.hit(position) == Some(id)
    }

    /// The flat list of draw commands, back-to-front. A zero-area node,
    /// or one that draws nothing (`Panel` with `a == 0`), is skipped.
    pub fn draw_list(&self, pointer: [f32; 2]) -> Vec<DrawCommand> {
        let mut out = Vec::new();
        if let Some(root) = self.root {
            self.collect_draw(root, pointer, &mut out);
        }
        out
    }

    fn collect_draw(&self, id: NodeId, pointer: [f32; 2], out: &mut Vec<DrawCommand>) {
        let node = &self.nodes[id];
        let rect = node.computed;
        if rect.w > 0.0 && rect.h > 0.0 {
            match &node.widget {
                Widget::Panel { color } => {
                    if color.a > 0.0 {
                        out.push(DrawCommand {
                            rect,
                            kind: DrawKind::Fill(*color),
                        });
                    }
                }
                Widget::Label { text, color } => out.push(DrawCommand {
                    rect,
                    kind: DrawKind::Text {
                        text: text.clone(),
                        color: *color,
                    },
                }),
                Widget::Button { text, bg, hot_bg } => {
                    let fill = if self.is_hot(id, pointer) {
                        *hot_bg
                    } else {
                        *bg
                    };
                    out.push(DrawCommand {
                        rect,
                        kind: DrawKind::Fill(fill),
                    });
                    out.push(DrawCommand {
                        rect,
                        kind: DrawKind::Text {
                            text: text.clone(),
                            color: Color::WHITE,
                        },
                    });
                }
                Widget::Image { texture, tint } => out.push(DrawCommand {
                    rect,
                    kind: DrawKind::Image {
                        texture: *texture,
                        tint: *tint,
                    },
                }),
            }
        }
        for &child in &node.children {
            self.collect_draw(child, pointer, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(color: Color) -> Widget {
        Widget::Panel { color }
    }

    fn button() -> Widget {
        Widget::Button {
            text: "OK".into(),
            bg: Color::rgb(0.2, 0.2, 0.2),
            hot_bg: Color::rgb(0.4, 0.4, 0.4),
        }
    }

    #[test]
    fn center_anchor_centres_the_box_in_the_parent() {
        let mut ui = Ui::new(200.0, 100.0);
        let id = ui.add(
            None,
            Style {
                anchor: Anchor::Center,
                offset: [0.0, 0.0],
                size: [40.0, 20.0],
            },
            panel(Color::WHITE),
        );
        ui.layout();
        let r = ui.rect_of(id).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (80.0, 40.0, 40.0, 20.0));
    }

    #[test]
    fn corner_anchors_and_offsets_place_the_box() {
        let mut ui = Ui::new(200.0, 100.0);
        let br = ui.add(
            None,
            Style {
                anchor: Anchor::BottomRight,
                offset: [-10.0, -10.0],
                size: [30.0, 30.0],
            },
            panel(Color::WHITE),
        );
        ui.layout();
        let r = ui.rect_of(br).unwrap();
        // anchor at (190, 90) after offset; box is right/bottom-aligned.
        assert_eq!((r.x, r.y), (160.0, 60.0));
    }

    #[test]
    fn children_lay_out_relative_to_the_parent_rect() {
        let mut ui = Ui::new(400.0, 400.0);
        let parent = ui.add(
            None,
            Style {
                anchor: Anchor::TopLeft,
                offset: [50.0, 50.0],
                size: [100.0, 100.0],
            },
            panel(Color::WHITE),
        );
        let child = ui.add(
            Some(parent),
            Style {
                anchor: Anchor::TopLeft,
                offset: [10.0, 10.0],
                size: [20.0, 20.0],
            },
            panel(Color::WHITE),
        );
        ui.layout();
        let r = ui.rect_of(child).unwrap();
        assert_eq!((r.x, r.y), (60.0, 60.0));
    }

    #[test]
    fn hit_returns_the_topmost_node() {
        let mut ui = Ui::new(100.0, 100.0);
        let bg = ui.add(
            None,
            Style {
                anchor: Anchor::TopLeft,
                offset: [0.0, 0.0],
                size: [100.0, 100.0],
            },
            panel(Color::WHITE),
        );
        let fg = ui.add(
            Some(bg),
            Style {
                anchor: Anchor::TopLeft,
                offset: [10.0, 10.0],
                size: [20.0, 20.0],
            },
            panel(Color::WHITE),
        );
        ui.layout();
        assert_eq!(ui.hit([15.0, 15.0]), Some(fg));
        assert_eq!(ui.hit([50.0, 50.0]), Some(bg));
        assert_eq!(ui.hit([200.0, 200.0]), None);
    }

    #[test]
    fn button_click_needs_press_and_release_on_the_same_node() {
        let mut ui = Ui::new(100.0, 100.0);
        let btn = ui.add(
            None,
            Style {
                anchor: Anchor::TopLeft,
                offset: [0.0, 0.0],
                size: [50.0, 50.0],
            },
            button(),
        );
        ui.layout();

        // Press inside, release inside → clicked.
        assert!(
            ui.interact(PointerInput {
                position: [10.0, 10.0],
                pressed: true,
                released: false,
            })
            .is_empty()
        );
        let clicked = ui.interact(PointerInput {
            position: [10.0, 10.0],
            pressed: false,
            released: true,
        });
        assert_eq!(clicked, vec![btn]);

        // Press inside, release outside → nothing.
        ui.interact(PointerInput {
            position: [10.0, 10.0],
            pressed: true,
            released: false,
        });
        let clicked = ui.interact(PointerInput {
            position: [90.0, 90.0],
            pressed: false,
            released: true,
        });
        assert!(clicked.is_empty());
    }

    #[test]
    fn draw_list_is_back_to_front_and_skips_invisible_nodes() {
        let mut ui = Ui::new(100.0, 100.0);
        let bg = ui.add(
            None,
            Style {
                anchor: Anchor::TopLeft,
                offset: [0.0, 0.0],
                size: [100.0, 100.0],
            },
            panel(Color::rgb(0.1, 0.1, 0.1)),
        );
        // Transparent panel → no draw command.
        ui.add(
            Some(bg),
            Style {
                anchor: Anchor::TopLeft,
                offset: [0.0, 0.0],
                size: [10.0, 10.0],
            },
            panel(Color::TRANSPARENT),
        );
        let label = ui.add(
            Some(bg),
            Style {
                anchor: Anchor::Center,
                offset: [0.0, 0.0],
                size: [40.0, 10.0],
            },
            Widget::Label {
                text: "hi".into(),
                color: Color::WHITE,
            },
        );
        ui.layout();

        let list = ui.draw_list([0.0, 0.0]);
        assert_eq!(list.len(), 2, "bg fill + label text; transparent skipped");
        assert!(matches!(list[0].kind, DrawKind::Fill(_)));
        match &list[1].kind {
            DrawKind::Text { text, .. } => assert_eq!(text, "hi"),
            other => panic!("expected text, got {other:?}"),
        }
        assert_eq!(ui.rect_of(label).unwrap().w, 40.0);
    }

    #[test]
    fn clear_resets_the_tree() {
        let mut ui = Ui::new(10.0, 10.0);
        ui.add(None, Style::default(), panel(Color::WHITE));
        ui.clear();
        ui.layout();
        assert!(ui.draw_list([0.0, 0.0]).is_empty());
        assert_eq!(ui.hit([0.0, 0.0]), None);
    }
}
