//! A pin-and-wire node canvas: boxes with input pins on the left and
//! output pins on the right, joined by curved wires. Used by the Shader
//! Graph window; the caller owns the graph and applies the
//! [`CanvasEvent`]s this returns.
//!
//! Drag a node to move it, drag empty space to pan, drag from an output
//! pin to an input pin to wire them, drag a wire off an input pin to move
//! or remove it, right-click empty space for the caller's add menu, and
//! press Delete (or Backspace) to remove the selected node.

use crate::theme;

/// Width of a node.
const NODE_WIDTH: f32 = 168.0;
/// Height of a node's title bar.
const HEADER: f32 = 28.0;
/// Height of one pin row.
const ROW: f32 = 22.0;
/// Radius of a drawn pin.
const PIN_RADIUS: f32 = 5.0;
/// How close the pointer must be to a pin to grab or drop on it.
const PIN_REACH: f32 = 11.0;

/// One node as the canvas draws it.
#[derive(Debug, Clone)]
pub(crate) struct CanvasNode {
    /// Unique among the nodes.
    pub(crate) id: u32,
    /// Top-left corner, in graph space.
    pub(crate) position: [f32; 2],
    /// The title.
    pub(crate) title: String,
    /// A line under the title, such as a value.
    pub(crate) detail: Option<String>,
    /// Input pin names, top to bottom.
    pub(crate) inputs: Vec<String>,
    /// Output pin names, top to bottom.
    pub(crate) outputs: Vec<String>,
    /// The colour bar on its left edge.
    pub(crate) accent: egui::Color32,
    /// Drawn with a warning edge: the node an error names.
    pub(crate) warning: bool,
    /// Each input pin's colour (by what it carries); empty for the
    /// canvas's own.
    pub(crate) input_colors: Vec<egui::Color32>,
    /// Each output pin's colour; empty for the canvas's own. Wires take
    /// their output's colour.
    pub(crate) output_colors: Vec<egui::Color32>,
    /// Drawn with a green edge: it just ran (a game in the Scene View).
    pub(crate) lit: bool,
}

/// A note drawn behind the nodes.
#[derive(Debug, Clone)]
pub(crate) struct CanvasComment {
    /// Top-left corner, in graph space.
    pub(crate) position: [f32; 2],
    /// Width and height.
    pub(crate) size: [f32; 2],
    /// What it says, in its title strip.
    pub(crate) text: String,
}

/// What the canvas shows: nodes, wires, comments and what is selected.
#[derive(Clone, Copy)]
pub(crate) struct CanvasContent<'a> {
    pub(crate) nodes: &'a [CanvasNode],
    pub(crate) wires: &'a [CanvasWire],
    pub(crate) comments: &'a [CanvasComment],
    pub(crate) selected: Option<u32>,
    pub(crate) selected_comment: Option<usize>,
}

/// Height of a comment's title strip, which moves it.
const COMMENT_HEADER: f32 = 26.0;
/// The resize grip in a comment's bottom-right corner.
const COMMENT_GRIP: f32 = 16.0;

/// A wire from output `from_pin` of node `from` to input `to_pin` of node
/// `to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CanvasWire {
    pub(crate) from: u32,
    pub(crate) from_pin: u8,
    pub(crate) to: u32,
    pub(crate) to_pin: u8,
}

/// What the user did on the canvas this frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CanvasEvent {
    /// A node was dragged to `position` (graph space).
    Moved { node: u32, position: [f32; 2] },
    /// A node was clicked, or empty space (`None`).
    Selected(Option<u32>),
    /// A new wire.
    Connected(CanvasWire),
    /// The wire into input `to_pin` of `to` was pulled off.
    Disconnected { to: u32, to_pin: u8 },
    /// Delete was pressed with `node` selected.
    Delete(u32),
    /// Comment `index` was clicked.
    CommentSelected(usize),
    /// Comment `index` was dragged to `position`.
    CommentMoved { index: usize, position: [f32; 2] },
    /// Comment `index` was resized to `size`.
    CommentResized { index: usize, size: [f32; 2] },
}

/// Where a wire drag started: an output pin, in graph space.
#[derive(Debug, Clone, Copy, PartialEq)]
struct WireDrag {
    from: u32,
    from_pin: u8,
}

fn node_height(node: &CanvasNode) -> f32 {
    let detail = if node.detail.is_some() {
        ROW - 4.0
    } else {
        0.0
    };
    HEADER + detail + ROW * node.inputs.len().max(node.outputs.len()).max(1) as f32 + 6.0
}

fn first_row(node: &CanvasNode) -> f32 {
    HEADER
        + if node.detail.is_some() {
            ROW - 4.0
        } else {
            0.0
        }
}

/// Screen position of input pin `pin` of a node whose rect is `rect`.
fn input_pin(node: &CanvasNode, rect: egui::Rect, pin: usize) -> egui::Pos2 {
    egui::pos2(
        rect.left(),
        rect.top() + first_row(node) + ROW * (pin as f32 + 0.5),
    )
}

/// Screen position of output pin `pin`.
fn output_pin(node: &CanvasNode, rect: egui::Rect, pin: usize) -> egui::Pos2 {
    egui::pos2(
        rect.right(),
        rect.top() + first_row(node) + ROW * (pin as f32 + 0.5),
    )
}

fn wire_shape(from: egui::Pos2, to: egui::Pos2, color: egui::Color32) -> egui::Shape {
    // Forward wires bend gently; backward ones loop out far enough to
    // read as going back.
    let gap = to.x - from.x;
    let bend = if gap >= 0.0 {
        (gap * 0.5).max(12.0)
    } else {
        (-gap * 0.5).max(40.0)
    };
    egui::epaint::CubicBezierShape::from_points_stroke(
        [
            from,
            from + egui::vec2(bend, 0.0),
            to - egui::vec2(bend, 0.0),
            to,
        ],
        false,
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(2.0, color),
    )
    .into()
}

/// Graph-space position of the canvas's centre, for placing new nodes.
pub(crate) fn centre(ctx: &egui::Context, id: egui::Id, size: egui::Vec2) -> [f32; 2] {
    let pan = ctx
        .data(|data| data.get_temp::<egui::Vec2>(id.with("pan")))
        .unwrap_or(egui::Vec2::ZERO);
    let at = size * 0.5 - pan - egui::vec2(NODE_WIDTH * 0.5, 40.0);
    [at.x.round(), at.y.round()]
}

/// Moves the view back to the graph's origin.
pub(crate) fn reset_view(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|data| data.insert_temp(id.with("pan"), egui::Vec2::ZERO));
}

/// Draws the canvas, `size` large, and returns what the user did.
/// `add_menu` fills the right-click menu on empty space; it gets the
/// graph-space point clicked.
pub(crate) fn show(
    ui: &mut egui::Ui,
    id: egui::Id,
    size: egui::Vec2,
    content: &CanvasContent<'_>,
    add_menu: &mut dyn FnMut(&mut egui::Ui, [f32; 2]),
) -> Vec<CanvasEvent> {
    let CanvasContent {
        nodes,
        wires,
        comments,
        selected,
        selected_comment,
    } = *content;
    let palette = theme::NIGHT;
    let pan_id = id.with("pan");
    let drag_id = id.with("wire_drag");
    let menu_at_id = id.with("menu_at");
    let mut pan = ui
        .ctx()
        .data(|data| data.get_temp::<egui::Vec2>(pan_id))
        .unwrap_or(egui::Vec2::ZERO);
    let mut drag = ui.ctx().data(|data| data.get_temp::<WireDrag>(drag_id));
    let mut events = Vec::new();

    let (rect, background) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    if background.dragged() && drag.is_none() {
        pan += background.drag_delta();
    }
    if background.clicked() {
        events.push(CanvasEvent::Selected(None));
    }
    if background.secondary_clicked()
        && let Some(pointer) = background.interact_pointer_pos()
    {
        let at = pointer - rect.min - pan;
        ui.ctx()
            .data_mut(|data| data.insert_temp(menu_at_id, [at.x.round(), at.y.round()]));
    }
    background.context_menu(|ui| {
        theme::style_context_menu(ui);
        let at = ui
            .ctx()
            .data(|data| data.get_temp::<[f32; 2]>(menu_at_id))
            .unwrap_or([0.0, 0.0]);
        add_menu(ui, at);
    });

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, egui::CornerRadius::same(8), palette.shell);
    // A faint dot grid that moves with the pan, so panning reads as motion.
    let spacing = 24.0;
    let offset = egui::vec2(pan.x.rem_euclid(spacing), pan.y.rem_euclid(spacing));
    let mut y = rect.top() + offset.y;
    while y < rect.bottom() {
        let mut x = rect.left() + offset.x;
        while x < rect.right() {
            painter.circle_filled(egui::pos2(x, y), 1.0, palette.rule);
            x += spacing;
        }
        y += spacing;
    }

    let origin = rect.min + pan;
    let rect_of = |node: &CanvasNode| {
        egui::Rect::from_min_size(
            origin + egui::vec2(node.position[0], node.position[1]),
            egui::vec2(NODE_WIDTH, node_height(node)),
        )
    };
    let find = |id: u32| nodes.iter().find(|node| node.id == id);

    for (index, comment) in comments.iter().enumerate() {
        let area = egui::Rect::from_min_size(
            origin + egui::vec2(comment.position[0], comment.position[1]),
            egui::vec2(comment.size[0], comment.size[1]),
        );
        if !area.intersects(rect) {
            continue;
        }
        let chosen = selected_comment == Some(index);
        painter.rect_filled(
            area,
            egui::CornerRadius::same(10),
            palette.panel.gamma_multiply(0.85),
        );
        let header = egui::Rect::from_min_size(area.min, egui::vec2(area.width(), COMMENT_HEADER));
        painter.rect_filled(
            header,
            egui::CornerRadius {
                nw: 10,
                ne: 10,
                sw: 0,
                se: 0,
            },
            if chosen {
                palette.accent_fill
            } else {
                palette.hover
            },
        );
        painter.text(
            header.left_center() + egui::vec2(10.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &comment.text,
            egui::FontId::proportional(12.5),
            palette.muted,
        );
        let handle = ui.interact(
            header.intersect(rect),
            id.with(("comment", index)),
            egui::Sense::click_and_drag(),
        );
        if handle.clicked() || handle.drag_started() {
            events.push(CanvasEvent::CommentSelected(index));
        }
        if handle.dragged() && drag.is_none() {
            let to = egui::vec2(comment.position[0], comment.position[1]) + handle.drag_delta();
            events.push(CanvasEvent::CommentMoved {
                index,
                position: [to.x.round(), to.y.round()],
            });
        }
        let grip = egui::Rect::from_min_size(
            area.max - egui::Vec2::splat(COMMENT_GRIP),
            egui::Vec2::splat(COMMENT_GRIP),
        );
        painter.line_segment(
            [
                grip.right_top() + egui::vec2(-3.0, 5.0),
                grip.left_bottom() + egui::vec2(5.0, -3.0),
            ],
            egui::Stroke::new(1.5, palette.subtle),
        );
        let resize = ui.interact(
            grip.intersect(rect),
            id.with(("comment-size", index)),
            egui::Sense::drag(),
        );
        if resize.dragged() {
            let size = egui::vec2(comment.size[0], comment.size[1]) + resize.drag_delta();
            events.push(CanvasEvent::CommentResized {
                index,
                size: [size.x.max(120.0).round(), size.y.max(60.0).round()],
            });
        }
    }

    for wire in wires {
        let (Some(from), Some(to)) = (find(wire.from), find(wire.to)) else {
            continue;
        };
        let start = output_pin(from, rect_of(from), usize::from(wire.from_pin));
        let end = input_pin(to, rect_of(to), usize::from(wire.to_pin));
        let color = from
            .output_colors
            .get(usize::from(wire.from_pin))
            .copied()
            .unwrap_or(palette.muted);
        painter.add(wire_shape(start, end, color));
    }

    let pointer = ui.ctx().pointer_latest_pos();
    let mut hovered_input = None;
    for node in nodes {
        let node_rect = rect_of(node);
        if !node_rect.intersects(rect) {
            continue;
        }
        let is_selected = selected == Some(node.id);
        let body = ui.interact(
            node_rect.intersect(rect),
            id.with(("node", node.id)),
            egui::Sense::click_and_drag(),
        );
        if body.clicked() || body.drag_started() {
            events.push(CanvasEvent::Selected(Some(node.id)));
        }
        if body.dragged() && drag.is_none() {
            let to = egui::vec2(node.position[0], node.position[1]) + body.drag_delta();
            events.push(CanvasEvent::Moved {
                node: node.id,
                position: [to.x.round(), to.y.round()],
            });
        }
        let fill = if is_selected {
            palette.accent_fill
        } else if body.hovered() {
            palette.hover
        } else {
            palette.raised
        };
        painter.rect_filled(node_rect, egui::CornerRadius::same(8), fill);
        if node.warning || node.lit {
            painter.rect_stroke(
                node_rect,
                egui::CornerRadius::same(8),
                egui::Stroke::new(
                    1.5,
                    if node.warning {
                        palette.danger
                    } else {
                        palette.success
                    },
                ),
                egui::StrokeKind::Inside,
            );
        }
        painter.rect_filled(
            egui::Rect::from_min_size(node_rect.min, egui::vec2(NODE_WIDTH, 4.0)),
            egui::CornerRadius {
                nw: 8,
                ne: 8,
                sw: 0,
                se: 0,
            },
            node.accent,
        );
        painter.text(
            node_rect.left_top() + egui::vec2(12.0, HEADER * 0.5 + 2.0),
            egui::Align2::LEFT_CENTER,
            &node.title,
            egui::FontId::proportional(13.0),
            palette.text,
        );
        if let Some(detail) = &node.detail {
            painter.text(
                node_rect.left_top() + egui::vec2(12.0, HEADER + (ROW - 4.0) * 0.5 - 2.0),
                egui::Align2::LEFT_CENTER,
                detail,
                egui::FontId::proportional(11.5),
                palette.muted,
            );
        }
        for (pin, name) in node.inputs.iter().enumerate() {
            let at = input_pin(node, node_rect, pin);
            let wired = wires
                .iter()
                .any(|wire| wire.to == node.id && usize::from(wire.to_pin) == pin);
            let near = pointer.is_some_and(|pointer| pointer.distance(at) <= PIN_REACH);
            if near {
                hovered_input = Some((node.id, pin as u8));
            }
            let color = match node.input_colors.get(pin) {
                Some(color) if wired => *color,
                Some(color) => color.gamma_multiply(0.5),
                None if wired => palette.accent,
                None => palette.subtle,
            };
            painter.circle_filled(at, PIN_RADIUS, if near { palette.text } else { color });
            painter.text(
                at + egui::vec2(12.0, 0.0),
                egui::Align2::LEFT_CENTER,
                name,
                egui::FontId::proportional(11.5),
                palette.muted,
            );
            let grab = ui.interact(
                egui::Rect::from_center_size(at, egui::Vec2::splat(PIN_REACH * 2.0)),
                id.with(("in", node.id, pin)),
                egui::Sense::drag(),
            );
            if grab.drag_started()
                && let Some(wire) = wires
                    .iter()
                    .find(|wire| wire.to == node.id && usize::from(wire.to_pin) == pin)
            {
                // Pulling a wire off an input picks it up from its source.
                events.push(CanvasEvent::Disconnected {
                    to: node.id,
                    to_pin: pin as u8,
                });
                drag = Some(WireDrag {
                    from: wire.from,
                    from_pin: wire.from_pin,
                });
            }
        }
        for (pin, name) in node.outputs.iter().enumerate() {
            let at = output_pin(node, node_rect, pin);
            let near = pointer.is_some_and(|pointer| pointer.distance(at) <= PIN_REACH);
            let color = node
                .output_colors
                .get(pin)
                .copied()
                .unwrap_or(palette.accent);
            painter.circle_filled(at, PIN_RADIUS, if near { palette.text } else { color });
            painter.text(
                at - egui::vec2(12.0, 0.0),
                egui::Align2::RIGHT_CENTER,
                name,
                egui::FontId::proportional(11.5),
                palette.muted,
            );
            let grab = ui.interact(
                egui::Rect::from_center_size(at, egui::Vec2::splat(PIN_REACH * 2.0)),
                id.with(("out", node.id, pin)),
                egui::Sense::drag(),
            );
            if grab.drag_started() {
                drag = Some(WireDrag {
                    from: node.id,
                    from_pin: pin as u8,
                });
            }
        }
    }

    if let Some(active) = drag {
        let released = ui.input(|input| input.pointer.any_released());
        if let (Some(from), Some(pointer)) = (find(active.from), pointer) {
            let start = output_pin(from, rect_of(from), usize::from(active.from_pin));
            let color = from
                .output_colors
                .get(usize::from(active.from_pin))
                .copied()
                .unwrap_or(palette.accent);
            painter.add(wire_shape(start, pointer, color));
        }
        if released {
            if let Some((to, to_pin)) = hovered_input
                && to != active.from
            {
                events.push(CanvasEvent::Connected(CanvasWire {
                    from: active.from,
                    from_pin: active.from_pin,
                    to,
                    to_pin,
                }));
            }
            drag = None;
        }
    }

    let typing = ui.ctx().egui_wants_keyboard_input();
    if let Some(node) = selected
        && !typing
        && background.ctx.input(|input| {
            input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace)
        })
        && ui.rect_contains_pointer(rect)
    {
        events.push(CanvasEvent::Delete(node));
    }

    ui.ctx().data_mut(|data| {
        data.insert_temp(pan_id, pan);
        match drag {
            Some(drag) => {
                data.insert_temp(drag_id, drag);
            }
            None => data.remove::<WireDrag>(drag_id),
        }
    });
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(inputs: usize, outputs: usize, detail: bool) -> CanvasNode {
        CanvasNode {
            id: 1,
            position: [0.0, 0.0],
            title: "Add".into(),
            detail: detail.then(|| "1.0".into()),
            inputs: vec!["A".into(); inputs],
            outputs: vec!["Out".into(); outputs],
            accent: egui::Color32::WHITE,
            warning: false,
            input_colors: Vec::new(),
            output_colors: Vec::new(),
            lit: false,
        }
    }

    #[test]
    fn pins_sit_on_the_edges_one_row_apart_below_the_title() {
        let shape = node(3, 1, false);
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(NODE_WIDTH, 100.0));
        let first = input_pin(&shape, rect, 0);
        let second = input_pin(&shape, rect, 1);
        assert_eq!(first.x, 0.0);
        assert_eq!(second.y - first.y, ROW);
        assert!(first.y > HEADER);
        assert_eq!(output_pin(&shape, rect, 0).x, NODE_WIDTH);
        assert!(node_height(&shape) >= HEADER + 3.0 * ROW);
        assert!(
            input_pin(&node(1, 1, true), rect, 0).y > first.y,
            "detail line"
        );
    }
}
