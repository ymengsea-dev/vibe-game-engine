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
//! - No glyph rendering — a text-carrying widget emits a [`DrawKind::Text`]
//!   command with the string and size; `engine::app` turns it into glyphs.
//! - No scrolling or grid — anchor + offset + fixed size, plus [`Stack`]
//!   for simple vertical and horizontal lists.
//!
//! ## Values and typing
//!
//! A [`Widget::Slider`] or [`Widget::Toggle`] changes itself when the player
//! uses it and reports the new value through [`Ui::take_changes`]. Whoever
//! owns the tree decides what the value means; [`Node::variable`] names the
//! gameplay variable a scene element is bound to. A [`Widget::TextInput`]
//! being typed in is [`Ui::editing`]; feed it characters with [`Ui::type_text`].
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

/// RGBA colour as seen on screen (sRGB), components in `0.0..=1.0`. Alpha is
/// straight, not premultiplied.
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

    /// An opaque colour from RGB.
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

/// Which way a [`Stack`] lists its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StackDirection {
    /// Top to bottom.
    #[default]
    Vertical,
    /// Left to right.
    Horizontal,
}

/// Lists a node's visible children one after another instead of placing each
/// by its own anchor and offset. Children keep their own size.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stack {
    /// Direction of the list.
    pub direction: StackDirection,
    /// Pixels between neighbouring children.
    pub gap: f32,
    /// Pixels between the node's edge and its children.
    pub padding: f32,
}

/// A value the player changed by using a widget this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueChange {
    /// The slider or toggle.
    pub node: NodeId,
    /// Its new value; a toggle reports `1.0` for on and `0.0` for off.
    pub value: f32,
}

/// Text height in pixels when a node does not set its own.
pub const DEFAULT_FONT_SIZE: f32 = 16.0;

/// Longest text a [`Widget::TextInput`] accepts while typing, in bytes.
pub const MAX_TEXT_INPUT_BYTES: usize = 1024;

/// `value`'s position between `min` and `max`, as `0.0..=1.0`. An empty or
/// reversed range reads as `0.0`.
fn fraction(value: f32, min: f32, max: f32) -> f32 {
    if max > min {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
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
    /// A bar filled by how far `value` lies between `min` and `max`.
    ProgressBar {
        /// Current value.
        value: f32,
        /// Value at which the bar is empty.
        min: f32,
        /// Value at which the bar is full.
        max: f32,
        /// Colour of the empty part.
        track: Color,
        /// Colour of the filled part.
        fill: Color,
    },
    /// A horizontal value the player drags, or nudges with focus and arrows.
    Slider {
        /// Current value.
        value: f32,
        /// Value at the left end.
        min: f32,
        /// Value at the right end.
        max: f32,
        /// Values snap to multiples of this from `min`; `0` means no snapping.
        step: f32,
        /// Colour of the empty part.
        track: Color,
        /// Colour of the filled part.
        fill: Color,
    },
    /// A box with a caption that flips on and off when clicked.
    Toggle {
        /// Caption beside the box.
        text: String,
        /// Current state.
        on: bool,
        /// Colour of the box.
        off_color: Color,
        /// Colour of the mark shown while on.
        on_color: Color,
    },
    /// A single-line field the player types into.
    TextInput {
        /// What has been typed.
        text: String,
        /// Shown while the field is empty and not being typed in.
        placeholder: String,
        /// Background colour.
        bg: Color,
        /// Text colour.
        color: Color,
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
    /// Height of this node's text in pixels.
    pub font_size: f32,
    /// Roundness of this node's corners in pixels.
    pub corner_radius: f32,
    /// Whether this node's text uses the heavier font.
    pub bold: bool,
    /// Caption colour for a [`Widget::Button`] or [`Widget::Toggle`]; white
    /// when unset. Other widgets carry their own text colour.
    pub text_color: Option<Color>,
    /// Lists the children instead of placing them by their anchors.
    pub stack: Option<Stack>,
    /// The gameplay variable this node's value follows, if any.
    pub variable: Option<String>,
    /// Whether this node (and so its children) is shown; see [`Ui::set_visible`].
    visible: bool,
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
    screen_root: Option<NodeId>,
    screen: [f32; 2],
    /// The node a pointer press started over, tracked so a release only
    /// counts as a click if it lands on the same node.
    press_target: Option<NodeId>,
    /// The focused node, if any. Always a focusable, existing id.
    focus: Option<NodeId>,
    /// How the focused node is outlined.
    ring: FocusRing,
    /// The slider being dragged.
    dragging: Option<NodeId>,
    /// The text field being typed in.
    editing: Option<NodeId>,
    /// Whether focus last moved by keyboard, gamepad or game code. A mouse
    /// user can see what they clicked, so they get no ring.
    ring_visible: bool,
    /// Values the player changed since [`Ui::take_changes`].
    changes: Vec<ValueChange>,
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
    /// Fill the rect with a solid colour, with rounded corners.
    RoundedFill {
        /// Fill colour.
        color: Color,
        /// Corner radius in pixels.
        radius: f32,
    },
    /// Draw a line just inside the rect's edge.
    Outline {
        /// Line colour.
        color: Color,
        /// Corner radius in pixels.
        radius: f32,
        /// Line thickness in pixels.
        width: f32,
    },
    /// Draw `texture` into the rect, multiplied by `tint`.
    Image {
        /// Texture asset.
        texture: AssetId,
        /// Tint colour.
        tint: Color,
        /// Corner radius in pixels.
        radius: f32,
    },
    /// Draw `text` in `color`, vertically centred within the rect.
    Text {
        /// The string.
        text: String,
        /// Text colour.
        color: Color,
        /// Font size in pixels.
        size: f32,
        /// Whether to use the heavier font.
        bold: bool,
        /// Horizontal placement.
        align: TextAlign,
    },
}

/// How a [`DrawKind::Text`] sits in its rect horizontally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    /// From the left edge, wrapping at the right edge.
    Left,
    /// Centred, wrapping at the rect's width.
    Center,
    /// One line that keeps its end in view when too long, for typed text.
    Tail,
}

impl Ui {
    /// An empty tree sized for a `width` x `height` screen.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            nodes: Vec::new(),
            root: None,
            screen_root: None,
            screen: [width.max(0.0), height.max(0.0)],
            press_target: None,
            focus: None,
            ring: FocusRing::default(),
            dragging: None,
            editing: None,
            ring_visible: false,
            changes: Vec::new(),
        }
    }

    /// Updates the screen size used as the root node's parent rect.
    pub fn set_screen(&mut self, width: f32, height: f32) {
        self.screen = [width.max(0.0), height.max(0.0)];
        if let Some(root) = self.screen_root {
            self.nodes[root].style.size = self.screen;
        }
    }

    /// Removes every node. Existing [`NodeId`]s become invalid.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.root = None;
        self.screen_root = None;
        self.press_target = None;
        // Ids are about to be reused by the next tree; keeping the old
        // one would focus an unrelated node.
        self.focus = None;
        self.dragging = None;
        self.editing = None;
        self.changes.clear();
    }

    /// Adds a node under `parent` (or as the root when `parent` is
    /// `None` and there is no root yet) and returns its id.
    ///
    /// A second `None` parent after a root exists attaches to the root.
    /// An out-of-range `parent` also attaches to the root (or becomes
    /// the root if there isn't one) rather than panicking.
    pub fn add(&mut self, parent: Option<NodeId>, style: Style, widget: Widget) -> NodeId {
        let id = self.nodes.len();
        let focusable = matches!(
            widget,
            Widget::Button { .. }
                | Widget::Slider { .. }
                | Widget::Toggle { .. }
                | Widget::TextInput { .. }
        );
        self.nodes.push(Node {
            style,
            widget,
            focusable,
            font_size: DEFAULT_FONT_SIZE,
            corner_radius: 0.0,
            bold: false,
            text_color: None,
            stack: None,
            variable: None,
            visible: true,
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

    /// Adds a screen-anchored element. Call before any regular root is added;
    /// the first call creates a transparent full-screen parent that follows resize.
    pub fn add_screen(&mut self, style: Style, widget: Widget) -> NodeId {
        let root = match self.screen_root {
            Some(root) => root,
            None => {
                let root = self.add(
                    None,
                    Style {
                        size: self.screen,
                        ..Style::default()
                    },
                    Widget::Panel {
                        color: Color::TRANSPARENT,
                    },
                );
                self.screen_root = Some(root);
                root
            }
        };
        self.add(Some(root), style, widget)
    }

    /// Shows or hides a node. A hidden node and all its children are not
    /// drawn, not hit-tested and not focusable; layout is unchanged. Focus on a
    /// node that becomes hidden is dropped. Returns `false` if `id` does not exist.
    pub fn set_visible(&mut self, id: NodeId, visible: bool) -> bool {
        let Some(node) = self.nodes.get_mut(id) else {
            return false;
        };
        node.visible = visible;
        let reachable = self.focusables();
        if self.focus.is_some_and(|focus| !reachable.contains(&focus)) {
            self.focus = None;
        }
        // The editing check ignores `focusable`: a field stops being typed in
        // only because it can no longer be seen.
        if self.editing.is_some_and(|node| !self.is_shown(node)) {
            self.editing = None;
        }
        if self.dragging.is_some_and(|node| !self.is_shown(node)) {
            self.dragging = None;
        }
        true
    }

    /// Whether the node itself is marked visible (its ancestors may still hide
    /// it). `false` for a missing node.
    pub fn is_visible(&self, id: NodeId) -> bool {
        self.nodes.get(id).is_some_and(|node| node.visible)
    }

    /// Whether `id` is drawn: it and every ancestor are visible.
    fn is_shown(&self, id: NodeId) -> bool {
        fn reaches(ui: &Ui, from: NodeId, id: NodeId) -> bool {
            let node = &ui.nodes[from];
            node.visible
                && (from == id || node.children.iter().any(|&child| reaches(ui, child, id)))
        }
        self.root.is_some_and(|root| reaches(self, root, id))
    }

    /// How many nodes exist; every id below this is valid.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
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
        let root_rect = Self::place(self.nodes[root].style, screen);
        let mut pending = vec![(root, root_rect)];
        while let Some((id, rect)) = pending.pop() {
            self.nodes[id].computed = rect;
            let node = &self.nodes[id];
            match node.stack {
                None => {
                    for &child in node.children.iter().rev() {
                        pending.push((child, Self::place(self.nodes[child].style, rect)));
                    }
                }
                Some(stack) => {
                    let mut cursor = stack.padding;
                    for &child in &node.children {
                        let child_node = &self.nodes[child];
                        let [w, h] = child_node.style.size;
                        let (x, y) = match stack.direction {
                            StackDirection::Vertical => (rect.x + stack.padding, rect.y + cursor),
                            StackDirection::Horizontal => (rect.x + cursor, rect.y + stack.padding),
                        };
                        pending.push((child, Rect { x, y, w, h }));
                        // A hidden child takes no room in the list.
                        if child_node.visible {
                            cursor += stack.gap
                                + match stack.direction {
                                    StackDirection::Vertical => h,
                                    StackDirection::Horizontal => w,
                                };
                        }
                    }
                }
            }
        }
    }

    /// Where `style` puts a node inside `parent`.
    fn place(style: Style, parent: Rect) -> Rect {
        let (fx, fy) = style.anchor.factors();
        let anchor_x = parent.x + parent.w * fx + style.offset[0];
        let anchor_y = parent.y + parent.h * fy + style.offset[1];
        Rect {
            x: anchor_x - style.size[0] * fx,
            y: anchor_y - style.size[1] * fy,
            w: style.size[0],
            h: style.size[1],
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
        if !node.visible {
            return None;
        }
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
                    self.ring_visible = true;
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
        if !node.visible {
            return;
        }
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
        self.ring_visible = true;
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
        self.ring_visible = true;
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
    ///
    /// A focused toggle flips and a focused text field starts typing, as a
    /// click on them would; neither is reported as a click.
    pub fn activate(&mut self) -> Option<NodeId> {
        let id = self.focus?;
        if !self.focusables().contains(&id) {
            return None;
        }
        match self.nodes.get(id)?.widget {
            Widget::Button { .. } => Some(id),
            Widget::Toggle { .. } => {
                self.flip_toggle(id);
                None
            }
            Widget::TextInput { .. } => {
                self.editing = Some(id);
                None
            }
            _ => None,
        }
    }

    /// The text field being typed in. While this is set, keyboard input
    /// belongs to the field rather than to the game.
    pub fn editing(&self) -> Option<NodeId> {
        self.editing
    }

    /// Feeds typed characters to the field being typed in. Backspace removes
    /// the last character, Escape stops typing, and Enter stops typing and
    /// returns the field. Characters arriving with no field being typed in
    /// are dropped.
    pub fn type_text(&mut self, typed: &str) -> Option<NodeId> {
        let id = self.editing?;
        let Widget::TextInput { text, .. } = &mut self.nodes.get_mut(id)?.widget else {
            self.editing = None;
            return None;
        };
        for character in typed.chars() {
            match character {
                '\u{8}' | '\u{7f}' => {
                    text.pop();
                }
                '\r' | '\n' => {
                    self.editing = None;
                    return Some(id);
                }
                '\u{1b}' => {
                    self.editing = None;
                    return None;
                }
                other if other.is_control() => {}
                other => {
                    if text.len() + other.len_utf8() <= MAX_TEXT_INPUT_BYTES {
                        text.push(other);
                    }
                }
            }
        }
        None
    }

    /// Moves the focused slider one step: right for a positive `direction`,
    /// left for a negative one. Returns whether a slider took the press, so
    /// the caller knows not to move focus instead. A slider with no step moves
    /// a twentieth of its range.
    pub fn nudge_focused(&mut self, direction: f32) -> bool {
        let Some(id) = self.focus else {
            return false;
        };
        let Some(Widget::Slider {
            value,
            min,
            max,
            step,
            ..
        }) = self.nodes.get(id).map(|node| &node.widget)
        else {
            return false;
        };
        let amount = if *step > 0.0 {
            *step
        } else {
            (*max - *min) / 20.0
        };
        self.change_slider(id, *value + amount * direction.signum());
        true
    }

    /// Sets a bar's or slider's value, or a toggle's state (on when above
    /// `0.5`), without reporting it as a player change. A slider being dragged
    /// keeps the player's value. Returns `false` for any other widget.
    pub fn set_value(&mut self, id: NodeId, new_value: f32) -> bool {
        let dragged = self.dragging == Some(id);
        match self.nodes.get_mut(id).map(|node| &mut node.widget) {
            Some(Widget::ProgressBar { value, .. }) => *value = new_value,
            Some(Widget::Slider { value, .. }) => {
                if !dragged {
                    *value = new_value;
                }
            }
            Some(Widget::Toggle { on, .. }) => *on = new_value > 0.5,
            _ => return false,
        }
        true
    }

    /// The values the player changed since the last call, oldest first.
    pub fn take_changes(&mut self) -> Vec<ValueChange> {
        std::mem::take(&mut self.changes)
    }

    fn flip_toggle(&mut self, id: NodeId) {
        if let Some(Widget::Toggle { on, .. }) = self.nodes.get_mut(id).map(|node| &mut node.widget)
        {
            *on = !*on;
            let value = if *on { 1.0 } else { 0.0 };
            self.changes.push(ValueChange { node: id, value });
        }
    }

    /// Clamps and snaps `requested`, and reports it if the slider moved.
    fn change_slider(&mut self, id: NodeId, requested: f32) {
        let Some(Widget::Slider {
            value,
            min,
            max,
            step,
            ..
        }) = self.nodes.get_mut(id).map(|node| &mut node.widget)
        else {
            return;
        };
        let snapped = if *step > 0.0 {
            *min + ((requested - *min) / *step).round() * *step
        } else {
            requested
        };
        let next = snapped.clamp(*min, (*max).max(*min));
        if next != *value {
            *value = next;
            self.changes.push(ValueChange {
                node: id,
                value: next,
            });
        }
    }

    /// Feeds one frame of pointer input and returns the ids of every
    /// [`Widget::Button`] that completed a click this frame (pressed and
    /// released over the same button).
    pub fn interact(&mut self, input: PointerInput) -> Vec<NodeId> {
        let over = self.hit(input.position);

        if input.pressed {
            self.press_target = over;
            // Pressing anywhere else stops typing.
            self.editing =
                over.filter(|&id| matches!(self.nodes[id].widget, Widget::TextInput { .. }));
            self.dragging =
                over.filter(|&id| matches!(self.nodes[id].widget, Widget::Slider { .. }));
            for id in [self.editing, self.dragging].into_iter().flatten() {
                if self.nodes[id].focusable {
                    self.focus = Some(id);
                    self.ring_visible = false;
                }
            }
        }
        if let Some(id) = self.dragging {
            let rect = self.nodes[id].computed;
            if let Widget::Slider { min, max, .. } = self.nodes[id].widget
                && rect.w > 0.0
            {
                let along = ((input.position[0] - rect.x) / rect.w).clamp(0.0, 1.0);
                self.change_slider(id, min + along * (max - min));
            }
        }

        let mut clicked = Vec::new();
        if input.released {
            if let (Some(pressed), Some(released)) = (self.press_target, over)
                && pressed == released
            {
                match self.nodes[released].widget {
                    Widget::Button { .. } => clicked.push(released),
                    Widget::Toggle { .. } => self.flip_toggle(released),
                    _ => {}
                }
                // A clicked button takes focus, so picking up a gamepad
                // mid-menu resumes from what the mouse last touched
                // rather than from wherever focus happened to be.
                if self.nodes[released].focusable {
                    self.focus = Some(released);
                    self.ring_visible = false;
                }
            }
            self.press_target = None;
            self.dragging = None;
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
        let hot = self.hit(pointer);
        if let Some(root) = self.root {
            self.collect_draw(root, hot, &mut out);
        }
        self.collect_focus_ring(&mut out);
        out
    }

    /// Appends the focused node's outline.
    ///
    /// Last in the list, not next to the node it surrounds: a sibling
    /// painted later would otherwise cover the ring on exactly the
    /// overlapping menus where it matters most.
    fn collect_focus_ring(&self, out: &mut Vec<DrawCommand>) {
        if !self.ring_visible {
            return;
        }
        let Some(id) = self.focus.filter(|id| self.focusables().contains(id)) else {
            return;
        };
        let Some(node) = self.nodes.get(id) else {
            return;
        };
        let rect = node.computed;
        let width = self.ring.thickness;
        if width <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        // Drawn inside the node's box, so a ring never overlaps its
        // neighbour and layout does not shift when focus moves.
        out.push(DrawCommand {
            rect,
            kind: DrawKind::Outline {
                color: self.ring.color,
                radius: node.corner_radius,
                width,
            },
        });
    }

    fn collect_draw(&self, id: NodeId, hot: Option<NodeId>, out: &mut Vec<DrawCommand>) {
        let node = &self.nodes[id];
        if !node.visible {
            return;
        }
        let rect = node.computed;
        let radius = node.corner_radius;
        let size = node.font_size;
        let bold = node.bold;
        if rect.w > 0.0 && rect.h > 0.0 {
            match &node.widget {
                Widget::Panel { color } => {
                    if color.a > 0.0 {
                        out.push(fill(rect, *color, radius));
                    }
                }
                Widget::Label { text, color } => out.push(DrawCommand {
                    rect,
                    kind: DrawKind::Text {
                        text: text.clone(),
                        color: *color,
                        size,
                        bold,
                        align: TextAlign::Left,
                    },
                }),
                Widget::Button { text, bg, hot_bg } => {
                    let background = if hot == Some(id) { *hot_bg } else { *bg };
                    out.push(fill(rect, background, radius));
                    out.push(DrawCommand {
                        rect,
                        kind: DrawKind::Text {
                            text: text.clone(),
                            color: node.text_color.unwrap_or(Color::WHITE),
                            size,
                            bold,
                            align: TextAlign::Center,
                        },
                    });
                }
                Widget::Image { texture, tint } => out.push(DrawCommand {
                    rect,
                    kind: DrawKind::Image {
                        texture: *texture,
                        tint: *tint,
                        radius,
                    },
                }),
                Widget::ProgressBar {
                    value,
                    min,
                    max,
                    track,
                    fill: filled,
                } => {
                    let along = fraction(*value, *min, *max);
                    push_bar(out, rect, along, *track, *filled, radius);
                }
                Widget::Slider {
                    value,
                    min,
                    max,
                    track,
                    fill: filled,
                    ..
                } => {
                    let along = fraction(*value, *min, *max);
                    let bar_height = (rect.h / 3.0).clamp(2.0, rect.h);
                    let bar = Rect {
                        y: rect.y + (rect.h - bar_height) * 0.5,
                        h: bar_height,
                        ..rect
                    };
                    push_bar(out, bar, along, *track, *filled, bar_height * 0.5);
                    let knob = rect.h.min(rect.w);
                    out.push(fill(
                        Rect {
                            x: rect.x + (rect.w - knob) * along,
                            w: knob,
                            ..rect
                        },
                        Color::WHITE,
                        knob * 0.5,
                    ));
                }
                Widget::Toggle {
                    text,
                    on,
                    off_color,
                    on_color,
                } => {
                    let side = rect.h.min(rect.w);
                    let check = Rect {
                        w: side,
                        h: side,
                        y: rect.y + (rect.h - side) * 0.5,
                        ..rect
                    };
                    out.push(fill(check, *off_color, radius));
                    if *on {
                        let inset = side * 0.2;
                        out.push(fill(
                            Rect {
                                x: check.x + inset,
                                y: check.y + inset,
                                w: side - inset * 2.0,
                                h: side - inset * 2.0,
                            },
                            *on_color,
                            (radius - inset).max(0.0),
                        ));
                    }
                    let gap = side + 8.0;
                    out.push(DrawCommand {
                        rect: Rect {
                            x: rect.x + gap,
                            w: (rect.w - gap).max(0.0),
                            ..rect
                        },
                        kind: DrawKind::Text {
                            text: text.clone(),
                            color: node.text_color.unwrap_or(Color::WHITE),
                            size,
                            bold,
                            align: TextAlign::Left,
                        },
                    });
                }
                Widget::TextInput {
                    text,
                    placeholder,
                    bg,
                    color,
                } => {
                    out.push(fill(rect, *bg, radius));
                    let typing = self.editing == Some(id);
                    let (shown, shown_color) = if typing {
                        (format!("{text}_"), *color)
                    } else if text.is_empty() {
                        (
                            placeholder.clone(),
                            Color {
                                a: color.a * 0.5,
                                ..*color
                            },
                        )
                    } else {
                        (text.clone(), *color)
                    };
                    const FIELD_PADDING: f32 = 6.0;
                    out.push(DrawCommand {
                        rect: Rect {
                            x: rect.x + FIELD_PADDING,
                            w: (rect.w - FIELD_PADDING * 2.0).max(0.0),
                            ..rect
                        },
                        kind: DrawKind::Text {
                            text: shown,
                            color: shown_color,
                            size,
                            bold,
                            align: TextAlign::Tail,
                        },
                    });
                }
            }
        }
        for &child in &node.children {
            self.collect_draw(child, hot, out);
        }
    }
}

/// A solid rectangle, square unless `radius` says otherwise.
fn fill(rect: Rect, color: Color, radius: f32) -> DrawCommand {
    DrawCommand {
        rect,
        kind: if radius > 0.0 {
            DrawKind::RoundedFill { color, radius }
        } else {
            DrawKind::Fill(color)
        },
    }
}

/// Draws a bar's empty part, then its filled part over the left `along`. The
/// filled part never gets narrower than its corners, so a low value still
/// reads as a rounded end.
fn push_bar(
    out: &mut Vec<DrawCommand>,
    rect: Rect,
    along: f32,
    track: Color,
    filled: Color,
    radius: f32,
) {
    out.push(fill(rect, track, radius));
    if along > 0.0 {
        let radius = radius.min(rect.h * 0.5);
        let width = (rect.w * along).max((radius * 2.0).min(rect.w));
        out.push(fill(Rect { w: width, ..rect }, filled, radius));
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
    fn screen_elements_anchor_independently_and_follow_resize() {
        let mut ui = Ui::new(200.0, 100.0);
        let left = ui.add_screen(
            Style {
                anchor: Anchor::TopLeft,
                offset: [10.0, 10.0],
                size: [20.0, 10.0],
            },
            button(),
        );
        let right = ui.add_screen(
            Style {
                anchor: Anchor::TopRight,
                offset: [-10.0, 10.0],
                size: [20.0, 10.0],
            },
            button(),
        );
        ui.layout();
        assert_eq!(ui.rect_of(left).unwrap().x, 10.0);
        assert_eq!(ui.rect_of(right).unwrap().x, 170.0);
        assert_eq!(ui.focusables(), vec![left, right]);

        ui.set_screen(400.0, 100.0);
        ui.layout();
        assert_eq!(ui.rect_of(left).unwrap().x, 10.0);
        assert_eq!(ui.rect_of(right).unwrap().x, 370.0);
        assert_eq!(ui.hit([375.0, 15.0]), Some(right));
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

        assert_eq!(focused.len(), unfocused.len() + 1);
        let ring = focused.last().unwrap();
        assert!(matches!(ring.kind, DrawKind::Outline { .. }));
        assert_eq!(ring.rect, ui.rect_of(up).unwrap());
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
    fn hidden_ui_node_is_not_drawn_or_clicked() {
        let mut ui = Ui::new(200.0, 200.0);
        let root = ui.add(None, Style::default(), panel(Color::TRANSPARENT));
        let group = ui.add(
            Some(root),
            Style {
                size: [100.0, 100.0],
                ..Style::default()
            },
            panel(Color::WHITE),
        );
        let child = ui.add(
            Some(group),
            Style {
                size: [60.0, 40.0],
                ..Style::default()
            },
            button(),
        );
        ui.layout();
        let centre = centre(ui.rect_of(child).unwrap());
        let click = |ui: &mut Ui| {
            ui.interact(PointerInput {
                position: centre,
                pressed: true,
                released: true,
            })
        };
        assert_eq!(click(&mut ui), vec![child]);
        assert!(!ui.draw_list(centre).is_empty());

        assert!(ui.set_visible(group, false));
        assert!(!ui.is_visible(group));
        assert!(click(&mut ui).is_empty());
        assert!(ui.draw_list(centre).is_empty(), "no focus ring either");
        assert!(ui.focusables().is_empty());
        assert_eq!(ui.focus(), None, "focus dropped with the node");

        ui.set_visible(group, true);
        assert_eq!(click(&mut ui), vec![child]);
        assert!(!ui.draw_list(centre).is_empty());
        assert!(!ui.set_visible(99, true));
    }

    fn press(ui: &mut Ui, position: [f32; 2]) {
        ui.interact(PointerInput {
            position,
            pressed: true,
            released: false,
        });
    }

    fn release(ui: &mut Ui, position: [f32; 2]) {
        ui.interact(PointerInput {
            position,
            pressed: false,
            released: true,
        });
    }

    fn box_at(x: f32, y: f32, w: f32, h: f32) -> Style {
        Style {
            offset: [x, y],
            size: [w, h],
            ..Style::default()
        }
    }

    fn slider(step: f32) -> Widget {
        Widget::Slider {
            value: 0.0,
            min: 0.0,
            max: 10.0,
            step,
            track: Color::WHITE,
            fill: Color::WHITE,
        }
    }

    fn field() -> Widget {
        Widget::TextInput {
            text: String::new(),
            placeholder: "Name".into(),
            bg: Color::WHITE,
            color: Color::WHITE,
        }
    }

    fn toggle() -> Widget {
        Widget::Toggle {
            text: "Music".into(),
            on: false,
            off_color: Color::WHITE,
            on_color: Color::WHITE,
        }
    }

    #[test]
    fn stack_lists_visible_children_with_gap_and_padding() {
        let mut ui = Ui::new(400.0, 400.0);
        let list = ui.add(None, box_at(10.0, 20.0, 200.0, 200.0), panel(Color::WHITE));
        ui.node_mut(list).unwrap().stack = Some(Stack {
            direction: StackDirection::Vertical,
            gap: 4.0,
            padding: 8.0,
        });
        // Their own offsets must not count inside a list.
        let first = ui.add(Some(list), box_at(99.0, 99.0, 50.0, 30.0), button());
        let hidden = ui.add(Some(list), box_at(0.0, 0.0, 50.0, 30.0), button());
        let last = ui.add(Some(list), box_at(0.0, 0.0, 50.0, 30.0), button());
        ui.set_visible(hidden, false);
        ui.layout();
        let first_rect = ui.rect_of(first).unwrap();
        assert_eq!((first_rect.x, first_rect.y), (18.0, 28.0));
        let last_rect = ui.rect_of(last).unwrap();
        assert_eq!(last_rect.y, 28.0 + 30.0 + 4.0, "hidden child takes no room");

        ui.node_mut(list).unwrap().stack = Some(Stack {
            direction: StackDirection::Horizontal,
            gap: 4.0,
            padding: 0.0,
        });
        ui.layout();
        assert_eq!(ui.rect_of(last).unwrap().x, 10.0 + 50.0 + 4.0);
    }

    #[test]
    fn dragging_a_slider_snaps_clamps_and_reports_each_change() {
        let mut ui = Ui::new(200.0, 100.0);
        let id = ui.add(None, box_at(0.0, 0.0, 100.0, 20.0), slider(1.0));
        ui.layout();
        press(&mut ui, [42.0, 10.0]);
        assert_eq!(
            ui.take_changes(),
            vec![ValueChange {
                node: id,
                value: 4.0
            }]
        );
        // Still held, now past the end, and off the widget altogether.
        ui.interact(PointerInput {
            position: [500.0, 90.0],
            pressed: false,
            released: false,
        });
        assert_eq!(ui.take_changes()[0].value, 10.0);
        release(&mut ui, [500.0, 90.0]);
        assert!(ui.take_changes().is_empty());
        // Released: moving no longer changes it.
        ui.interact(PointerInput {
            position: [10.0, 10.0],
            pressed: false,
            released: false,
        });
        assert!(ui.take_changes().is_empty());
    }

    #[test]
    fn a_dragged_slider_ignores_outside_values_until_released() {
        let mut ui = Ui::new(200.0, 100.0);
        let id = ui.add(None, box_at(0.0, 0.0, 100.0, 20.0), slider(0.0));
        ui.layout();
        press(&mut ui, [50.0, 10.0]);
        ui.set_value(id, 1.0);
        let value = |ui: &Ui| match ui.node(id).unwrap().widget {
            Widget::Slider { value, .. } => value,
            _ => unreachable!(),
        };
        assert_eq!(value(&ui), 5.0);
        release(&mut ui, [50.0, 10.0]);
        ui.set_value(id, 1.0);
        assert_eq!(value(&ui), 1.0);
        assert!(ui.take_changes().iter().all(|change| change.value == 5.0));
    }

    #[test]
    fn focus_and_arrows_nudge_a_slider() {
        let mut ui = Ui::new(200.0, 100.0);
        let id = ui.add(None, box_at(0.0, 0.0, 100.0, 20.0), slider(0.0));
        assert!(!ui.nudge_focused(1.0), "nothing focused");
        ui.set_focus(Some(id));
        assert!(ui.nudge_focused(1.0));
        assert_eq!(ui.take_changes()[0].value, 0.5, "a twentieth of the range");
        assert!(ui.nudge_focused(-1.0));
        assert!(ui.nudge_focused(-1.0));
        assert_eq!(ui.take_changes().len(), 1, "stopped at the low end");
    }

    #[test]
    fn a_toggle_flips_on_click_and_on_activate_without_counting_as_a_click() {
        let mut ui = Ui::new(200.0, 100.0);
        let id = ui.add(None, box_at(0.0, 0.0, 100.0, 20.0), toggle());
        ui.layout();
        press(&mut ui, [5.0, 5.0]);
        let clicked = ui.interact(PointerInput {
            position: [5.0, 5.0],
            pressed: false,
            released: true,
        });
        assert!(clicked.is_empty());
        assert_eq!(ui.take_changes()[0].value, 1.0);
        assert_eq!(ui.focus(), Some(id));
        assert_eq!(ui.activate(), None);
        assert_eq!(ui.take_changes()[0].value, 0.0);
    }

    #[test]
    fn typing_goes_to_the_clicked_field_until_enter_escape_or_a_click_elsewhere() {
        let mut ui = Ui::new(200.0, 100.0);
        let id = ui.add(None, box_at(0.0, 0.0, 100.0, 20.0), field());
        ui.layout();
        assert_eq!(ui.type_text("ignored"), None);
        press(&mut ui, [5.0, 5.0]);
        assert_eq!(ui.editing(), Some(id));
        assert_eq!(ui.type_text("Abc\u{8}d"), None);
        let text = |ui: &Ui| match &ui.node(id).unwrap().widget {
            Widget::TextInput { text, .. } => text.clone(),
            _ => unreachable!(),
        };
        assert_eq!(text(&ui), "Abd");

        assert_eq!(ui.type_text("!\rlost"), Some(id));
        assert_eq!(text(&ui), "Abd!", "Enter ends the typing");
        assert_eq!(ui.editing(), None);

        press(&mut ui, [5.0, 5.0]);
        assert_eq!(ui.type_text("\u{1b}"), None);
        assert_eq!(ui.editing(), None);

        press(&mut ui, [5.0, 5.0]);
        press(&mut ui, [150.0, 90.0]);
        assert_eq!(ui.editing(), None);

        ui.set_focus(Some(id));
        assert_eq!(ui.activate(), None, "not a click");
        assert_eq!(ui.editing(), Some(id));
        ui.set_visible(id, false);
        assert_eq!(ui.editing(), None, "a hidden field stops typing");
    }

    #[test]
    fn typed_text_stops_at_the_size_limit() {
        let mut ui = Ui::new(200.0, 100.0);
        let id = ui.add(None, box_at(0.0, 0.0, 100.0, 20.0), field());
        ui.set_focus(Some(id));
        ui.activate();
        ui.type_text(&"x".repeat(MAX_TEXT_INPUT_BYTES + 10));
        assert!(matches!(
            &ui.node(id).unwrap().widget,
            Widget::TextInput { text, .. } if text.len() == MAX_TEXT_INPUT_BYTES
        ));
    }

    #[test]
    fn bars_fill_by_value_and_text_keeps_its_size_and_colour() {
        let mut ui = Ui::new(200.0, 100.0);
        let root = ui.add(
            None,
            box_at(0.0, 0.0, 200.0, 100.0),
            panel(Color::TRANSPARENT),
        );
        let bar = ui.add(
            Some(root),
            box_at(0.0, 0.0, 100.0, 10.0),
            Widget::ProgressBar {
                value: 15.0,
                min: 10.0,
                max: 20.0,
                track: Color::rgb(0.1, 0.1, 0.1),
                fill: Color::rgb(0.9, 0.1, 0.1),
            },
        );
        let caption = ui.add(Some(root), box_at(0.0, 20.0, 100.0, 20.0), button());
        ui.node_mut(caption).unwrap().font_size = 32.0;
        ui.node_mut(caption).unwrap().text_color = Some(Color::rgb(1.0, 0.0, 0.0));
        ui.layout();
        let list = ui.draw_list([f32::MIN, f32::MIN]);
        assert_eq!(list[1].rect.w, 50.0, "half way between min and max");
        assert!(matches!(
            &list[3].kind,
            DrawKind::Text { size, color, .. } if *size == 32.0 && color.r == 1.0 && color.g == 0.0
        ));
        ui.set_value(bar, 99.0);
        assert_eq!(
            ui.draw_list([f32::MIN, f32::MIN])[1].rect.w,
            100.0,
            "clamped"
        );
        ui.set_value(bar, 0.0);
        assert!(
            !matches!(ui.draw_list([f32::MIN, f32::MIN])[1].kind, DrawKind::Fill(c) if c.r == 0.9),
            "an empty bar draws no fill"
        );
    }

    #[test]
    fn a_field_shows_its_hint_when_empty_and_a_cursor_while_typing() {
        let mut ui = Ui::new(200.0, 100.0);
        let id = ui.add(None, box_at(0.0, 0.0, 100.0, 20.0), field());
        ui.layout();
        let shown = |ui: &Ui| {
            ui.draw_list([f32::MIN, f32::MIN])
                .iter()
                .find_map(|command| match &command.kind {
                    DrawKind::Text { text, .. } => Some(text.clone()),
                    _ => None,
                })
        };
        assert_eq!(shown(&ui).as_deref(), Some("Name"));
        ui.set_focus(Some(id));
        ui.activate();
        ui.type_text("Al");
        assert_eq!(shown(&ui).as_deref(), Some("Al_"));
    }

    #[test]
    fn a_mouse_click_shows_no_focus_ring_but_the_keyboard_does() {
        let (mut ui, [_, down, _, _]) = cross_menu();
        let plain = ui.draw_list([f32::MIN, f32::MIN]).len();
        press(&mut ui, [100.0, 150.0]);
        release(&mut ui, [100.0, 150.0]);
        assert_eq!(ui.focus(), Some(down));
        assert_eq!(ui.draw_list([f32::MIN, f32::MIN]).len(), plain);
        ui.focus_direction(FocusDirection::Up);
        assert_eq!(ui.draw_list([f32::MIN, f32::MIN]).len(), plain + 1);
    }

    #[test]
    fn corner_radius_reaches_every_fill_and_clamps_a_thin_bar() {
        let mut ui = Ui::new(200.0, 100.0);
        let bar = ui.add(
            None,
            box_at(0.0, 0.0, 100.0, 10.0),
            Widget::ProgressBar {
                value: 1.0,
                min: 0.0,
                max: 100.0,
                track: Color::WHITE,
                fill: Color::WHITE,
            },
        );
        ui.node_mut(bar).unwrap().corner_radius = 20.0;
        ui.layout();
        let list = ui.draw_list([f32::MIN, f32::MIN]);
        assert!(matches!(
            list[0].kind,
            DrawKind::RoundedFill { radius: 20.0, .. }
        ));
        assert!(matches!(
            list[1].kind,
            DrawKind::RoundedFill { radius: 5.0, .. }
        ));
        assert_eq!(list[1].rect.w, 10.0, "never narrower than its round ends");
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
