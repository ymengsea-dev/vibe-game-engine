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
//! - No scrolling, no flex/grid — anchor + offset + fixed size only.
//!
//! ## Focus
//!
//! A [`Ui`] tracks one focused node so a menu works with a keyboard or a
//! gamepad and no pointer at all. [`Ui::focus_next`] walks paint order
//! and wraps; [`Ui::focus_direction`] moves geometrically and stops at
//! the edge (see its docs for why the two differ). [`Ui::activate`]
//! clicks whatever is focused. The focused node gets a ring drawn over
//! everything else in [`Ui::draw_list`].
//!
//! Nothing here reads an input device: this crate has no
//! `engine_platform` dependency, so the frame loop (`engine::app`) is
//! what turns a d-pad press into a [`Ui::focus_direction`] call.

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

/// A rectangle's centre point, the reference used by directional focus.
fn centre(rect: Rect) -> [f32; 2] {
    [rect.x + rect.w * 0.5, rect.y + rect.h * 0.5]
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
    /// Whether focus can land here.
    ///
    /// [`Ui::add`] defaults it to "is this a [`Widget::Button`]", which
    /// is right for most trees. It is a node property rather than a
    /// widget one so a game can override both directions: a disabled
    /// button stops being reachable, and a [`Widget::Panel`] acting as a
    /// list row becomes reachable.
    pub focusable: bool,
    /// Child node ids, painted after (on top of) this node.
    children: Vec<NodeId>,
    /// Filled by [`Ui::layout`]; meaningless before the first call.
    computed: Rect,
}

/// The outline drawn around the focused node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FocusRing {
    /// Ring colour.
    pub color: Color,
    /// Border thickness in pixels. Zero draws no ring.
    pub thickness: f32,
}

impl Default for FocusRing {
    /// A 2px white outline.
    fn default() -> Self {
        Self {
            color: Color::WHITE,
            thickness: 2.0,
        }
    }
}

/// Which way [`Ui::focus_direction`] should look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    /// Towards smaller `y`.
    Up,
    /// Towards larger `y`.
    Down,
    /// Towards smaller `x`.
    Left,
    /// Towards larger `x`.
    Right,
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
    /// The focused node, if any. Always a focusable, existing id.
    focus: Option<NodeId>,
    /// How the focused node is outlined.
    ring: FocusRing,
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
            focus: None,
            ring: FocusRing::default(),
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
        // Ids are about to be reused by the next tree; keeping the old
        // one would focus an unrelated node.
        self.focus = None;
    }

    /// Adds a node under `parent` (or as the root when `parent` is
    /// `None` and there is no root yet) and returns its id.
    ///
    /// A second `None` parent after a root exists attaches to the root.
    /// An out-of-range `parent` also attaches to the root (or becomes
    /// the root if there isn't one) rather than panicking.
    pub fn add(&mut self, parent: Option<NodeId>, style: Style, widget: Widget) -> NodeId {
        let id = self.nodes.len();
        let focusable = matches!(widget, Widget::Button { .. });
        self.nodes.push(Node {
            style,
            widget,
            focusable,
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

    /// The focused node, if any.
    pub fn focus(&self) -> Option<NodeId> {
        self.focus
    }

    /// Moves focus to `id`, or clears it with `None`.
    ///
    /// Returns whether the request took effect: an id that does not
    /// exist, or exists but is not [`Node::focusable`], is refused and
    /// leaves the previous focus alone. Refusing rather than clearing
    /// means a game that focuses a stale id does not silently lose
    /// keyboard control of its menu.
    pub fn set_focus(&mut self, id: Option<NodeId>) -> bool {
        match id {
            None => {
                self.focus = None;
                true
            }
            Some(id) => {
                if self.nodes.get(id).is_some_and(|node| node.focusable) {
                    self.focus = Some(id);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Marks whether focus can land on `id`. Focusing out of a node that
    /// is currently focused clears the focus.
    ///
    /// Returns `false` if `id` does not exist.
    pub fn set_focusable(&mut self, id: NodeId, focusable: bool) -> bool {
        let Some(node) = self.nodes.get_mut(id) else {
            return false;
        };
        node.focusable = focusable;
        if !focusable && self.focus == Some(id) {
            self.focus = None;
        }
        true
    }

    /// The outline drawn around the focused node.
    pub fn focus_ring(&self) -> FocusRing {
        self.ring
    }

    /// Replaces the focus outline. A `thickness` of zero draws none.
    pub fn set_focus_ring(&mut self, ring: FocusRing) {
        self.ring = ring;
    }

    /// Every focusable node, in paint order (the order
    /// [`Ui::draw_list`] emits them, which is the order a player reads
    /// them).
    pub fn focusables(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        if let Some(root) = self.root {
            self.collect_focusables(root, &mut out);
        }
        out
    }

    fn collect_focusables(&self, id: NodeId, out: &mut Vec<NodeId>) {
        let node = &self.nodes[id];
        if node.focusable {
            out.push(id);
        }
        for &child in &node.children {
            self.collect_focusables(child, out);
        }
    }

    /// Moves focus to the next focusable node in paint order, **wrapping**
    /// past the last one back to the first — the tab-order convention.
    ///
    /// With nothing focused yet, focuses the first focusable node. A tree
    /// with no focusable nodes is a no-op. Returns the newly focused node.
    pub fn focus_next(&mut self) -> Option<NodeId> {
        self.step_focus(1)
    }

    /// [`Ui::focus_next`] backwards.
    pub fn focus_previous(&mut self) -> Option<NodeId> {
        self.step_focus(-1)
    }

    fn step_focus(&mut self, step: isize) -> Option<NodeId> {
        let order = self.focusables();
        if order.is_empty() {
            return None;
        }
        let next = match self
            .focus
            .and_then(|id| order.iter().position(|&o| o == id))
        {
            Some(index) => {
                let len = order.len() as isize;
                // `rem_euclid` so a backwards step from index 0 lands on
                // the last entry rather than going negative.
                ((index as isize + step).rem_euclid(len)) as usize
            }
            // Nothing focused (or focus sits outside the current tree):
            // start at whichever end the step is heading away from.
            None if step >= 0 => 0,
            None => order.len() - 1,
        };
        self.focus = Some(order[next]);
        self.focus
    }

    /// Moves focus to the nearest focusable node lying in `direction`,
    /// and **stops at the edge** rather than wrapping.
    ///
    /// The asymmetry with [`Ui::focus_next`] is deliberate. Tab order is
    /// a list, and a list wraps. Directional focus is spatial: pressing
    /// Right at the rightmost button and landing back on the leftmost one
    /// reads as the cursor teleporting, so nothing happens instead.
    ///
    /// With nothing focused yet, focuses the first focusable node — a
    /// player's first d-pad press should select something, not nothing.
    ///
    /// ## How "nearest" is decided
    ///
    /// Only nodes whose centre lies strictly beyond the focused node's
    /// centre along `direction`'s axis are candidates. Each is scored
    /// `axis_distance + 2 * cross_offset`, so a button a little further
    /// away but squarely in line beats one that is closer but far off to
    /// the side. Lowest score wins; equal scores break by [`NodeId`], so
    /// the choice never wanders between frames.
    ///
    /// Rectangles come from [`Ui::layout`] — call it first, or every
    /// node is still at the origin and the scoring is meaningless.
    pub fn focus_direction(&mut self, direction: FocusDirection) -> Option<NodeId> {
        let order = self.focusables();
        if order.is_empty() {
            return None;
        }
        let Some(current) = self.focus.filter(|id| order.contains(id)) else {
            self.focus = Some(order[0]);
            return self.focus;
        };

        let from = centre(self.nodes[current].computed);
        let mut best: Option<(f32, NodeId)> = None;
        for &id in &order {
            if id == current {
                continue;
            }
            let to = centre(self.nodes[id].computed);
            let (axis, cross) = match direction {
                FocusDirection::Up => (from[1] - to[1], (to[0] - from[0]).abs()),
                FocusDirection::Down => (to[1] - from[1], (to[0] - from[0]).abs()),
                FocusDirection::Left => (from[0] - to[0], (to[1] - from[1]).abs()),
                FocusDirection::Right => (to[0] - from[0], (to[1] - from[1]).abs()),
            };
            if axis <= 0.0 {
                continue;
            }
            let score = axis + 2.0 * cross;
            if best.is_none_or(|(best_score, _)| score < best_score) {
                best = Some((score, id));
            }
        }

        if let Some((_, id)) = best {
            self.focus = Some(id);
        }
        self.focus
    }

    /// Clicks the focused node, the way a pointer release over it would.
    ///
    /// Returns the activated id, or `None` when nothing is focused or the
    /// focused node is not a [`Widget::Button`] (a focusable panel can
    /// hold focus without being clickable). Feed the result into whatever
    /// already handles [`Ui::interact`]'s list — activation is a click,
    /// not a second kind of event.
    pub fn activate(&mut self) -> Option<NodeId> {
        let id = self.focus?;
        matches!(self.nodes.get(id)?.widget, Widget::Button { .. }).then_some(id)
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
                // A clicked button takes focus, so picking up a gamepad
                // mid-menu resumes from what the mouse last touched
                // rather than from wherever focus happened to be.
                if self.nodes[released].focusable {
                    self.focus = Some(released);
                }
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
        self.collect_focus_ring(&mut out);
        out
    }

    /// Appends the focused node's outline as four thin fills.
    ///
    /// Last in the list, not next to the node it surrounds: a sibling
    /// painted later would otherwise cover the ring on exactly the
    /// overlapping menus where it matters most. Four fills rather than a
    /// new [`DrawKind`] keeps every renderer that can already draw a
    /// rectangle able to draw a focus ring.
    fn collect_focus_ring(&self, out: &mut Vec<DrawCommand>) {
        let Some(id) = self.focus else { return };
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        let rect = node.computed;
        let t = self.ring.thickness;
        if t <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        // Drawn inside the node's box, so a ring never overlaps its
        // neighbour and layout does not shift when focus moves.
        let t = t.min(rect.w * 0.5).min(rect.h * 0.5);
        let sides = [
            Rect {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: t,
            },
            Rect {
                x: rect.x,
                y: rect.y + rect.h - t,
                w: rect.w,
                h: t,
            },
            Rect {
                x: rect.x,
                y: rect.y + t,
                w: t,
                h: (rect.h - 2.0 * t).max(0.0),
            },
            Rect {
                x: rect.x + rect.w - t,
                y: rect.y + t,
                w: t,
                h: (rect.h - 2.0 * t).max(0.0),
            },
        ];
        for side in sides {
            if side.w > 0.0 && side.h > 0.0 {
                out.push(DrawCommand {
                    rect: side,
                    kind: DrawKind::Fill(self.ring.color),
                });
            }
        }
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

    /// Four buttons in a plus shape around (100, 100), each 40x20:
    /// `up`, `down`, `left`, `right`. Laid out and ready to navigate.
    fn cross_menu() -> (Ui, [NodeId; 4]) {
        let mut ui = Ui::new(200.0, 200.0);
        let root = ui.add(
            None,
            Style {
                anchor: Anchor::TopLeft,
                offset: [0.0, 0.0],
                size: [200.0, 200.0],
            },
            panel(Color::TRANSPARENT),
        );
        let at = |x: f32, y: f32| Style {
            anchor: Anchor::TopLeft,
            offset: [x, y],
            size: [40.0, 20.0],
        };
        let up = ui.add(Some(root), at(80.0, 40.0), button());
        let down = ui.add(Some(root), at(80.0, 140.0), button());
        let left = ui.add(Some(root), at(20.0, 90.0), button());
        let right = ui.add(Some(root), at(140.0, 90.0), button());
        ui.layout();
        (ui, [up, down, left, right])
    }

    #[test]
    fn buttons_are_focusable_by_default_and_panels_are_not() {
        let mut ui = Ui::new(100.0, 100.0);
        let p = ui.add(None, Style::default(), panel(Color::WHITE));
        let b = ui.add(Some(p), Style::default(), button());
        assert!(!ui.node(p).unwrap().focusable);
        assert!(ui.node(b).unwrap().focusable);
        assert_eq!(ui.focusables(), vec![b]);
    }

    #[test]
    fn focus_moves_to_nearest_node_in_direction() {
        let (mut ui, [up, down, left, right]) = cross_menu();
        ui.set_focus(Some(left));

        assert_eq!(ui.focus_direction(FocusDirection::Right), Some(right));
        assert_eq!(
            ui.focus,
            Some(right),
            "the node squarely in line wins over the two off to the side"
        );

        ui.set_focus(Some(up));
        assert_eq!(ui.focus_direction(FocusDirection::Down), Some(down));
        ui.set_focus(Some(down));
        assert_eq!(ui.focus_direction(FocusDirection::Up), Some(up));
        ui.set_focus(Some(right));
        assert_eq!(ui.focus_direction(FocusDirection::Left), Some(left));
    }

    #[test]
    fn focus_direction_stops_at_the_edge() {
        let (mut ui, [_, _, left, _]) = cross_menu();
        ui.set_focus(Some(left));
        // Nothing lies further left; focus must stay put rather than
        // wrapping round to the right-hand button.
        assert_eq!(ui.focus_direction(FocusDirection::Left), Some(left));
    }

    #[test]
    fn first_directional_press_selects_something() {
        let (mut ui, _) = cross_menu();
        assert_eq!(ui.focus(), None);
        assert!(ui.focus_direction(FocusDirection::Down).is_some());
    }

    #[test]
    fn focus_wraps_at_the_edge() {
        let (mut ui, [up, down, left, right]) = cross_menu();
        assert_eq!(ui.focus_next(), Some(up));
        assert_eq!(ui.focus_next(), Some(down));
        assert_eq!(ui.focus_next(), Some(left));
        assert_eq!(ui.focus_next(), Some(right));
        assert_eq!(ui.focus_next(), Some(up), "tab order wraps");
        assert_eq!(ui.focus_previous(), Some(right), "and wraps backwards");
    }

    #[test]
    fn activate_fires_focused_node() {
        let (mut ui, [up, down, _, _]) = cross_menu();
        ui.set_focus(Some(down));
        assert_eq!(ui.activate(), Some(down));
        ui.set_focus(Some(up));
        assert_eq!(ui.activate(), Some(up), "and follows focus");
    }

    #[test]
    fn activate_without_focus_is_none() {
        let (mut ui, _) = cross_menu();
        assert_eq!(ui.activate(), None);
    }

    #[test]
    fn activate_on_a_focusable_non_button_is_none() {
        // A panel can hold focus (a list row) without being clickable.
        let mut ui = Ui::new(100.0, 100.0);
        let p = ui.add(None, Style::default(), panel(Color::WHITE));
        ui.set_focusable(p, true);
        assert!(ui.set_focus(Some(p)));
        assert_eq!(ui.activate(), None);
    }

    #[test]
    fn navigation_with_no_focusables_is_a_noop() {
        let mut ui = Ui::new(100.0, 100.0);
        let p = ui.add(None, Style::default(), panel(Color::WHITE));
        ui.add(Some(p), Style::default(), panel(Color::WHITE));
        ui.layout();

        assert_eq!(ui.focus_next(), None);
        assert_eq!(ui.focus_previous(), None);
        assert_eq!(ui.focus_direction(FocusDirection::Down), None);
        assert_eq!(ui.activate(), None);
        assert_eq!(ui.focus(), None);
    }

    #[test]
    fn set_focus_refuses_a_missing_or_unfocusable_id() {
        let (mut ui, [up, _, _, _]) = cross_menu();
        ui.set_focus(Some(up));
        assert!(!ui.set_focus(Some(999)), "a missing id is refused");
        assert_eq!(ui.focus(), Some(up), "and leaves the old focus alone");
        assert!(!ui.set_focus(Some(0)), "the root panel is not focusable");
        assert_eq!(ui.focus(), Some(up));
    }

    #[test]
    fn unfocusing_the_focused_node_drops_the_focus() {
        let (mut ui, [up, _, _, _]) = cross_menu();
        ui.set_focus(Some(up));
        ui.set_focusable(up, false);
        assert_eq!(ui.focus(), None);
        assert!(!ui.focusables().contains(&up));
    }

    #[test]
    fn clear_drops_focus() {
        let (mut ui, [up, _, _, _]) = cross_menu();
        ui.set_focus(Some(up));
        ui.clear();
        assert_eq!(ui.focus(), None, "ids are about to be reused");
    }

    #[test]
    fn focus_ring_paints_last_and_only_when_focused() {
        let (mut ui, [up, _, _, _]) = cross_menu();
        let unfocused = ui.draw_list([f32::MIN, f32::MIN]);
        ui.set_focus(Some(up));
        let focused = ui.draw_list([f32::MIN, f32::MIN]);

        assert_eq!(
            focused.len(),
            unfocused.len() + 4,
            "a ring is four edge fills"
        );
        let ring = &focused[focused.len() - 4..];
        let rect = ui.rect_of(up).unwrap();
        for command in ring {
            assert!(matches!(command.kind, DrawKind::Fill(_)));
            assert!(
                command.rect.x >= rect.x && command.rect.y >= rect.y,
                "the ring is drawn inside the focused node's box",
            );
        }
    }

    #[test]
    fn a_zero_thickness_ring_draws_nothing() {
        let (mut ui, [up, _, _, _]) = cross_menu();
        let plain = ui.draw_list([f32::MIN, f32::MIN]).len();
        ui.set_focus(Some(up));
        ui.set_focus_ring(FocusRing {
            color: Color::WHITE,
            thickness: 0.0,
        });
        assert_eq!(ui.draw_list([f32::MIN, f32::MIN]).len(), plain);
    }

    #[test]
    fn clicking_a_button_focuses_it() {
        let (mut ui, [_, down, _, _]) = cross_menu();
        let inside = [100.0, 150.0];
        ui.interact(PointerInput {
            position: inside,
            pressed: true,
            released: false,
        });
        let clicked = ui.interact(PointerInput {
            position: inside,
            pressed: false,
            released: true,
        });
        assert_eq!(clicked, vec![down]);
        assert_eq!(ui.focus(), Some(down));
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
