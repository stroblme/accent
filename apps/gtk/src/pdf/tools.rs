//! Pure PDF drawing-tool geometry for drags and the Select tool's handles.

/// What a drag over the page does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// No tool in hand: a drag selects the text under it, which is what it has always done.
    #[default]
    Read,
    /// Draw on the page.
    Pen,
    /// Draw over it in a wide translucent stroke that darkens rather than covers.
    Highlighter,
    /// Take a stroke off it.
    Eraser,
    /// A straight line, snapped to the axis it is close to.
    Line,
    /// A rectangle between the press and the release.
    Rect,
    /// A circle grown from the press outwards.
    Circle,
    /// Type onto the page: a click opens a box there, or on a text of ours types it again.
    Text,
    /// Take hold of strokes and texts: drag its middle to move it, an edge to stretch it, a corner
    /// to scale it.
    Select,
}

impl Mode {
    /// Whether a drag draws — everything but reading, erasing and selecting.
    pub fn draws(self) -> bool {
        matches!(
            self,
            Mode::Pen | Mode::Highlighter | Mode::Line | Mode::Rect | Mode::Circle
        )
    }

    /// Whether a drag is a shape: two points, the press and the release, rather than a path.
    pub fn shapes(self) -> bool {
        matches!(self, Mode::Line | Mode::Rect | Mode::Circle)
    }

    /// Window action for this tool, or `None` for Read. The ring, status bar and palette share
    /// its `ACTIONS` label to keep names consistent.
    pub fn action(self) -> Option<&'static str> {
        Some(match self {
            Mode::Read => return None,
            Mode::Pen => "win.pdf-pen",
            Mode::Highlighter => "win.pdf-highlighter",
            Mode::Eraser => "win.pdf-eraser",
            Mode::Line => "win.pdf-line",
            Mode::Rect => "win.pdf-rect",
            Mode::Circle => "win.pdf-circle",
            Mode::Text => "win.pdf-text",
            Mode::Select => "win.pdf-select",
        })
    }

    /// Whose width and colour a tool draws in: its own for the highlighter, the eraser and the
    /// text, whose width is its size, the pen's for the pen and every shape.
    pub fn style_owner(self) -> Mode {
        match self {
            Mode::Highlighter | Mode::Eraser | Mode::Text => self,
            _ => Mode::Pen,
        }
    }
}

impl crate::ring::Tool for Mode {
    /// The tools, in the order they sit on the ring: pen at the top, then clockwise. Each one's
    /// action and label are [`Mode::action`]'s, so a button says what the palette says.
    const TOOLS: &'static [(Mode, &'static str)] = &[
        (Mode::Pen, "tool-pen-symbolic"),
        (Mode::Highlighter, "tool-highlighter-symbolic"),
        (Mode::Eraser, "tool-eraser-symbolic"),
        (Mode::Line, "tool-line-symbolic"),
        (Mode::Rect, "tool-rect-symbolic"),
        (Mode::Circle, "tool-circle-symbolic"),
        (Mode::Text, "insert-text-symbolic"),
        (Mode::Select, "tool-select-symbolic"),
    ];

    fn action(self) -> Option<&'static str> {
        Mode::action(self)
    }

    fn none() -> Mode {
        Mode::Read
    }
}

/// What pressed the page, which decides what a drag does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hand {
    /// A stylus: its tip, or the eraser at its other end.
    Pen { eraser: bool },
    /// A finger on a touchscreen.
    Finger,
    /// A mouse or a touchpad.
    Mouse,
}

/// What a drag begun by `hand` does with `tool` in hand. A pen's tip draws and its eraser erases
/// under any tool; a mouse draws only while it may (`mouse_draws`: no pen on the seat, or the
/// preference), and selects text otherwise, but always places text, which is typed anyway, and
/// always uses the Select tool, which draws nothing. A finger is `None` under every tool: it
/// never draws, and its drag is the scrolled window's to pan, a long press first making it a
/// selection.
pub fn drag_of(tool: Mode, hand: Hand, mouse_draws: bool) -> Option<Mode> {
    match hand {
        Hand::Finger => None,
        _ if tool == Mode::Read => Some(Mode::Read),
        Hand::Pen { eraser: true } => Some(Mode::Eraser),
        Hand::Pen { eraser: false } => Some(tool),
        Hand::Mouse if mouse_draws || matches!(tool, Mode::Text | Mode::Select) => Some(tool),
        Hand::Mouse => Some(Mode::Read),
    }
}

/// Three widths per style, in page points — fine, the default, bold: the pen's, which the shapes
/// draw in too, the highlighter's, the eraser's reach, and typed text's size.
const WIDTHS: [(Mode, [f32; 3]); 4] = [
    (Mode::Pen, [0.75, 1.5, 3.0]),
    (Mode::Highlighter, [6.0, 10.5, 18.0]),
    (Mode::Eraser, [3.0, 6.0, 12.0]),
    (Mode::Text, [10.0, 14.0, 20.0]),
];

/// The three widths `tool` offers, by whose style it draws in; none for a tool without one.
pub fn widths(tool: Mode) -> Option<[f32; 3]> {
    match tool {
        Mode::Read | Mode::Select => None,
        _ => WIDTHS
            .iter()
            .find(|(m, _)| *m == tool.style_owner())
            .map(|(_, w)| *w),
    }
}

/// Which of `widths` is nearest to `stored`, so a hand-edited config, or one written before these
/// three were, still means one of them.
pub fn nearest(widths: [f32; 3], stored: f32) -> usize {
    let distance = |i: &usize| (widths[*i] - stored).abs();
    (0..widths.len())
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .unwrap_or(0)
}

/// The width `tool` draws at, or the eraser reaches, for the one the config holds.
pub fn width_of(tool: Mode, stored: f32) -> f32 {
    widths(tool).map_or(stored, |w| w[nearest(w, stored)])
}

/// How close to an axis, in degrees, a line has to be to snap onto it.
const SNAP_DEG: f32 = 4.0;
/// How near the pointer has to come to a stroke's edge, in page points, for the Select tool to
/// pick it.
pub(super) const PICK_RADIUS: f32 = 4.0;
/// The side of a Select handle, in pixels.
pub(super) const HANDLE: f32 = 8.0;
/// How far beyond the box's top-right corner the rotate handle sits, in pixels, as the diagram's
/// does (draw.io's `rotationHandleVSpacing`).
pub(super) const ROTATE_GAP: f32 = 12.0;

/// A line's end pulled onto the axis through its start when it is within [`SNAP_DEG`] of one.
pub fn snap(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let angle = dy.abs().atan2(dx.abs()).to_degrees();
    if angle < SNAP_DEG {
        (b.0, a.1)
    } else if angle > 90.0 - SNAP_DEG {
        (a.0, b.1)
    } else {
        b
    }
}

/// The shape a drag from `a` to `b` means under `mode`, or nothing for a drag too short to be
/// one — a click in a shape mode draws nothing — and for any other mode.
pub fn shape_of(mode: Mode, a: (f32, f32), b: (f32, f32)) -> Option<accent_core::pdf::Shape> {
    use accent_core::pdf::Shape;
    let radius = (b.0 - a.0).hypot(b.1 - a.1);
    if radius < 1.0 {
        return None;
    }
    match mode {
        Mode::Line => Some(Shape::Line { a, b }),
        Mode::Rect => Some(Shape::Rect(accent_core::pdf::Rect::from_corners(a, b))),
        Mode::Circle => Some(Shape::Circle { centre: a, radius }),
        _ => None,
    }
}

/// Where on the selection's box the Select tool took hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    Move,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// The ring beyond the top-right corner, which turns the stroke about its middle.
    Rotate,
}

impl Handle {
    /// The pointer over it, saying what a press there would do, as the diagram's handles say.
    pub fn cursor(self) -> &'static str {
        match self {
            Handle::Move => "move",
            Handle::Left => "w-resize",
            Handle::Right => "e-resize",
            Handle::Top => "n-resize",
            Handle::Bottom => "s-resize",
            Handle::TopLeft => "nw-resize",
            Handle::TopRight => "ne-resize",
            Handle::BottomLeft => "sw-resize",
            Handle::BottomRight => "se-resize",
            Handle::Rotate => "grab",
        }
    }
}

/// Which part of `bounds` is under `at`: a corner within `grip` of it, else an edge, else the
/// middle, else nothing.
pub fn handle_at(bounds: accent_core::pdf::Rect, at: (f32, f32), grip: f32) -> Option<Handle> {
    let near = |v: f32, edge: f32| (v - edge).abs() <= grip;
    if !bounds.grow(grip).contains(at) {
        return None;
    }
    let (l, r) = (near(at.0, bounds.left), near(at.0, bounds.right));
    let (t, b) = (near(at.1, bounds.top), near(at.1, bounds.bottom));
    Some(match (l, r, t, b) {
        (true, _, true, _) => Handle::TopLeft,
        (_, true, true, _) => Handle::TopRight,
        (true, _, _, true) => Handle::BottomLeft,
        (_, true, _, true) => Handle::BottomRight,
        (true, ..) => Handle::Left,
        (_, true, ..) => Handle::Right,
        (_, _, true, _) => Handle::Top,
        (_, _, _, true) => Handle::Bottom,
        _ => Handle::Move,
    })
}

/// Where the rotate handle of a stroke with box `b` sits, in page points at `scale` pixels a point.
pub fn rotate_handle(b: accent_core::pdf::Rect, scale: f32) -> (f32, f32) {
    (b.right + ROTATE_GAP / scale, b.top - ROTATE_GAP / scale)
}

/// The turn a drag of the rotate handle of a stroke with box `b` from `from` to `to` (page
/// points) gives it, about the box's middle: the bearing the pointer swept, to a tenth of a
/// degree, or in steps of 15° with `snap`. The diagram's ring keeps draw.io's steps by how far
/// out the pointer is (`diagram::geometry::rotation_to`); a stroke has no turn of its own to
/// land back on, so it turns freely unless Shift says otherwise.
pub fn turn_matrix(
    b: accent_core::pdf::Rect,
    from: (f32, f32),
    to: (f32, f32),
    snap: bool,
) -> accent_core::pdf::Matrix {
    let centre = ((b.left + b.right) / 2.0, (b.top + b.bottom) / 2.0);
    // Clockwise from straight up, as the page is read.
    let bearing = |(x, y): (f32, f32)| (x - centre.0).atan2(centre.1 - y).to_degrees();
    let step = if snap { 15.0 } else { 0.1 };
    let degrees = ((bearing(to) - bearing(from)) / step).round() * step;
    accent_core::pdf::rotation(centre, degrees)
}

/// The map a drag of `handle` by `(dx, dy)` page points applies to a stroke with this box.
///
/// The middle translates. An edge stretches its axis about the opposite edge. A corner scales
/// both axes alike, by the drag's projection onto the diagonal, about the opposite corner. A
/// scale never drops below 0.05, and an axis with no extent — a horizontal line's height — is
/// left alone rather than divided by.
pub fn drag_matrix(
    handle: Handle,
    b: accent_core::pdf::Rect,
    dx: f32,
    dy: f32,
) -> accent_core::pdf::Matrix {
    let (w, h) = (b.width(), b.height());
    let factor = |delta: f32, extent: f32| match extent > f32::EPSILON {
        true => (1.0 + delta / extent).max(0.05),
        false => 1.0,
    };
    let diagonal = |dx: f32, dy: f32| match w * w + h * h {
        d2 if d2 > f32::EPSILON => (1.0 + (dx * w + dy * h) / d2).max(0.05),
        _ => 1.0,
    };
    // Scaling about a fixed line: x' = s·x + (1 − s)·fixed.
    let about =
        |sx: f32, sy: f32, fx: f32, fy: f32| [sx, 0.0, 0.0, sy, (1.0 - sx) * fx, (1.0 - sy) * fy];
    match handle {
        Handle::Move => [1.0, 0.0, 0.0, 1.0, dx, dy],
        Handle::Right => about(factor(dx, w), 1.0, b.left, 0.0),
        Handle::Left => about(factor(-dx, w), 1.0, b.right, 0.0),
        Handle::Bottom => about(1.0, factor(dy, h), 0.0, b.top),
        Handle::Top => about(1.0, factor(-dy, h), 0.0, b.bottom),
        Handle::BottomRight => {
            let s = diagonal(dx, dy);
            about(s, s, b.left, b.top)
        }
        Handle::TopRight => {
            let s = diagonal(dx, -dy);
            about(s, s, b.left, b.bottom)
        }
        Handle::BottomLeft => {
            let s = diagonal(-dx, dy);
            about(s, s, b.right, b.top)
        }
        Handle::TopLeft => {
            let s = diagonal(-dx, -dy);
            about(s, s, b.right, b.bottom)
        }
        // A turn is no drag by an offset: see `turn_matrix`.
        Handle::Rotate => accent_core::pdf::IDENTITY,
    }
}

/// The translation `m` makes, cut back so that it takes `bounds` no further off a page of `size`
/// than it already is: a selection moved stops at the paper's edge.
pub fn clamp_move(
    bounds: accent_core::pdf::Rect,
    m: accent_core::pdf::Matrix,
    (width, height): (f32, f32),
) -> accent_core::pdf::Matrix {
    let along = |d: f32, low: f32, high: f32, extent: f32| {
        d.max((-low).min(0.0)).min((extent - high).max(0.0))
    };
    let dx = along(m[4], bounds.left, bounds.right, width);
    let dy = along(m[5], bounds.top, bounds.bottom, height);
    [m[0], m[1], m[2], m[3], dx, dy]
}

/// A stroke or a text of ours the Select tool holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Picked {
    /// The render thread's name for it, which follows it through a move.
    pub id: u32,
    pub points: Vec<(f32, f32)>,
    pub bounds: accent_core::pdf::Rect,
    pub style: accent_core::pdf::InkStyle,
    /// A text, which only moves, its corner going where a drag takes it: its size is the Text
    /// tool's, and an editor typing into a turned one would set it upright again.
    pub typed: bool,
}

impl Picked {
    /// A stroke as the page's list names it.
    pub fn stroke((id, ink): &accent_core::pdf::NamedInk) -> Picked {
        Picked {
            id: *id,
            points: ink.points.clone(),
            bounds: ink.bounds,
            style: ink.style,
            typed: false,
        }
    }

    /// A text of ours as the page's list names it: its box, and no path to draw a ghost along.
    pub fn text((id, text): &accent_core::pdf::NamedText) -> Picked {
        let [r, g, b] = text.typed.style.rgb;
        Picked {
            id: *id,
            points: Vec::new(),
            bounds: text.bounds,
            style: accent_core::pdf::InkStyle {
                width: 0.0,
                rgba: [r, g, b, 255],
                multiply: false,
            },
            typed: true,
        }
    }

    /// Where `m` takes it, as the render thread will draw it: a stroke whole, a text by its
    /// corner.
    pub fn under(&self, m: accent_core::pdf::Matrix) -> Picked {
        let b = self.bounds;
        match self.typed {
            true => {
                let (x, y) = accent_core::pdf::apply(m, (b.left, b.top));
                let (dx, dy) = (x - b.left, y - b.top);
                let bounds = accent_core::pdf::Rect {
                    left: x,
                    top: y,
                    right: b.right + dx,
                    bottom: b.bottom + dy,
                };
                Picked {
                    bounds,
                    ..self.clone()
                }
            }
            false => Picked {
                points: (self.points.iter())
                    .map(|&p| accent_core::pdf::apply(m, p))
                    .collect(),
                bounds: b.under(m),
                ..self.clone()
            },
        }
    }
}

/// What the Select tool holds: strokes and texts of ours of one page, and the drag being applied
/// to them.
pub struct Selection {
    pub page: usize,
    pub items: Vec<Picked>,
    /// Where the hand is holding it, while it is.
    pub handle: Option<Handle>,
    /// What the drag so far amounts to, painted over the page until the hand lets go.
    pub matrix: accent_core::pdf::Matrix,
}

impl Selection {
    /// `items` of `page`, held by nothing yet.
    pub fn of(page: usize, items: Vec<Picked>) -> Selection {
        Selection {
            page,
            items,
            handle: None,
            matrix: accent_core::pdf::IDENTITY,
        }
    }

    /// The box round every item.
    pub fn bounds(&self) -> accent_core::pdf::Rect {
        (self.items.iter().map(|item| item.bounds))
            .reduce(accent_core::pdf::Rect::union)
            .unwrap_or(accent_core::pdf::Rect::ZERO)
    }

    /// Whether it is texts alone, which only move: no handles and no ring.
    pub fn moves_only(&self) -> bool {
        self.items.iter().all(|item| item.typed)
    }

    /// The render thread's names for its items.
    pub fn ids(&self) -> Vec<u32> {
        self.items.iter().map(|item| item.id).collect()
    }
}

/// A free-hand lasso being drawn on `page`, in that page's own points; `add` (Ctrl) adds what it
/// takes to what the Select tool holds rather than holding that alone.
pub struct Lasso {
    pub page: usize,
    pub points: Vec<(f32, f32)>,
    pub add: bool,
}

/// A stroke as the widget holds it while it is being drawn.
pub struct Stroke {
    pub page: usize,
    /// The points, in that page's own points.
    pub points: Vec<(f32, f32)>,
    /// The tool that drew it, which is what the overlay is painted like.
    pub tool: Mode,
    /// Whether the hand has let go. A finished stroke stays painted until a tile carries it.
    pub done: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finger_never_draws_or_selects_and_a_pen_always_draws() {
        let tools = [
            Mode::Read,
            Mode::Pen,
            Mode::Highlighter,
            Mode::Eraser,
            Mode::Select,
        ];
        for tool in tools {
            for mouse_draws in [true, false] {
                assert_eq!(drag_of(tool, Hand::Finger, mouse_draws), None, "{tool:?}");
                let tip = drag_of(tool, Hand::Pen { eraser: false }, mouse_draws);
                assert_eq!(tip, Some(tool), "{tool:?}");
            }
        }
        let eraser = Hand::Pen { eraser: true };
        assert_eq!(drag_of(Mode::Pen, eraser, false), Some(Mode::Eraser));
        assert_eq!(drag_of(Mode::Read, eraser, false), Some(Mode::Read));
        assert_eq!(drag_of(Mode::Pen, Hand::Mouse, true), Some(Mode::Pen));
        assert_eq!(drag_of(Mode::Pen, Hand::Mouse, false), Some(Mode::Read));
        assert_eq!(drag_of(Mode::Text, Hand::Mouse, false), Some(Mode::Text));
        assert_eq!(
            drag_of(Mode::Select, Hand::Mouse, false),
            Some(Mode::Select)
        );
    }

    #[test]
    fn a_stored_width_draws_as_the_nearest_of_three() {
        // The widths before 2026-10-28, still in a config, land on their steps down.
        assert_eq!(width_of(Mode::Pen, 2.0), 1.5);
        assert_eq!(width_of(Mode::Line, 4.0), 3.0, "a shape draws in the pen's");
        assert_eq!(width_of(Mode::Highlighter, 14.0), 10.5);
        assert_eq!(width_of(Mode::Eraser, 4.0), 3.0);
        assert_eq!(widths(Mode::Select), None);
        let fresh = accent_core::config::DrawingConfig::default();
        for (tool, width) in [
            (Mode::Pen, fresh.pen_width),
            (Mode::Highlighter, fresh.highlighter_width),
            (Mode::Eraser, fresh.eraser_radius),
            (Mode::Text, fresh.text_size),
        ] {
            assert_eq!(
                width_of(tool, width),
                width,
                "{tool:?}'s default is one of its three"
            );
        }
    }

    #[test]
    fn a_line_snaps_to_the_axis_within_four_degrees() {
        assert_eq!(snap((0.0, 0.0), (100.0, 6.0)), (100.0, 0.0));
        assert_eq!(snap((0.0, 0.0), (100.0, 8.0)), (100.0, 8.0));
        assert_eq!(snap((0.0, 0.0), (6.0, 100.0)), (0.0, 100.0));
        assert_eq!(snap((0.0, 0.0), (8.0, 100.0)), (8.0, 100.0));
    }

    #[test]
    fn a_shape_is_normalised_and_a_click_is_not_one() {
        use accent_core::pdf::{Rect, Shape};
        let rect = shape_of(Mode::Rect, (50.0, 50.0), (10.0, 20.0));
        assert_eq!(
            rect,
            Some(Shape::Rect(Rect {
                left: 10.0,
                top: 20.0,
                right: 50.0,
                bottom: 50.0
            }))
        );
        let circle = shape_of(Mode::Circle, (0.0, 0.0), (3.0, 4.0));
        assert_eq!(
            circle,
            Some(Shape::Circle {
                centre: (0.0, 0.0),
                radius: 5.0
            })
        );
        assert_eq!(shape_of(Mode::Rect, (7.0, 7.0), (7.0, 7.5)), None);
        assert_eq!(shape_of(Mode::Pen, (0.0, 0.0), (9.0, 9.0)), None);
    }

    #[test]
    fn handles_map_to_matrices() {
        use accent_core::pdf::{Rect, apply};
        let b = Rect {
            left: 10.0,
            top: 20.0,
            right: 50.0,
            bottom: 60.0,
        };
        assert_eq!(handle_at(b, (50.0, 60.0), 3.0), Some(Handle::BottomRight));
        assert_eq!(handle_at(b, (30.0, 20.0), 3.0), Some(Handle::Top));
        assert_eq!(handle_at(b, (30.0, 40.0), 3.0), Some(Handle::Move));
        assert_eq!(handle_at(b, (100.0, 100.0), 3.0), None);

        let m = drag_matrix(Handle::Right, b, 40.0, 0.0);
        assert_eq!(apply(m, (50.0, 33.0)), (90.0, 33.0));
        assert_eq!(apply(m, (10.0, 33.0)), (10.0, 33.0));
        let m = drag_matrix(Handle::BottomRight, b, 40.0, 40.0);
        assert_eq!(apply(m, (50.0, 60.0)), (90.0, 100.0));
        assert_eq!(apply(m, (10.0, 20.0)), (10.0, 20.0));
        let m = drag_matrix(Handle::Move, b, 3.0, 4.0);
        assert_eq!(apply(m, (10.0, 20.0)), (13.0, 24.0));
        // A horizontal line has no height to stretch, and its edge drag leaves it a line.
        let flat = Rect {
            left: 0.0,
            top: 5.0,
            right: 10.0,
            bottom: 5.0,
        };
        assert_eq!(
            drag_matrix(Handle::Bottom, flat, 0.0, 9.0),
            accent_core::pdf::IDENTITY
        );
    }

    /// A move stops at the page's edges, and a box already over one moves along it but no
    /// further out.
    #[test]
    fn a_move_keeps_the_selection_on_its_page() {
        use accent_core::pdf::Rect;
        let b = Rect {
            left: 10.0,
            top: 20.0,
            right: 50.0,
            bottom: 60.0,
        };
        let page = (100.0, 100.0);
        let by = |dx: f32, dy: f32| [1.0, 0.0, 0.0, 1.0, dx, dy];
        assert_eq!(clamp_move(b, by(5.0, 5.0), page), by(5.0, 5.0));
        assert_eq!(clamp_move(b, by(80.0, 80.0), page), by(50.0, 40.0));
        assert_eq!(clamp_move(b, by(-30.0, -30.0), page), by(-10.0, -20.0));
        let over = Rect { left: -5.0, ..b };
        assert_eq!(clamp_move(over, by(-3.0, 0.0), page), by(0.0, 0.0));
        assert_eq!(clamp_move(over, by(3.0, 0.0), page), by(3.0, 0.0));
    }

    /// A selection's box holds every item; scaled by its corner, a stroke grows with it and a
    /// text only moves its corner, set as it was.
    #[test]
    fn a_selection_scales_its_strokes_and_moves_its_texts() {
        use accent_core::pdf::{InkStyle, Rect};
        let style = InkStyle {
            width: 1.0,
            rgba: [0, 0, 0, 255],
            multiply: false,
        };
        let stroke = Picked {
            id: 1,
            points: vec![(10.0, 20.0), (50.0, 60.0)],
            bounds: Rect::from_corners((10.0, 20.0), (50.0, 60.0)),
            style,
            typed: false,
        };
        let text = Picked {
            id: 2,
            points: Vec::new(),
            bounds: Rect::from_corners((60.0, 40.0), (90.0, 60.0)),
            style,
            typed: true,
        };
        let selection = Selection::of(0, vec![stroke, text]);
        let b = selection.bounds();
        assert_eq!(b, Rect::from_corners((10.0, 20.0), (90.0, 60.0)));
        assert!(!selection.moves_only());
        // Twice the size about the top-left corner.
        let m = drag_matrix(Handle::BottomRight, b, 80.0, 40.0);
        let [stroke, text] = [0, 1].map(|i| selection.items[i].under(m));
        assert_eq!(stroke.points, [(10.0, 20.0), (90.0, 100.0)]);
        assert_eq!(
            text.bounds,
            Rect::from_corners((110.0, 60.0), (140.0, 80.0))
        );
    }

    /// The ring dragged a quarter round the middle turns the stroke a quarter clockwise, about
    /// that middle; with Shift a little short of it still is a quarter.
    #[test]
    fn the_ring_turns_about_the_middle() {
        use accent_core::pdf::{Rect, apply};
        let b = Rect {
            left: 10.0,
            top: 20.0,
            right: 50.0,
            bottom: 60.0,
        };
        let near =
            |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01;
        // From straight above the middle (30, 40) to straight right of it.
        let m = turn_matrix(b, (30.0, 0.0), (70.0, 40.0), false);
        assert!(near(apply(m, (30.0, 20.0)), (50.0, 40.0)));
        assert!(near(apply(m, (30.0, 40.0)), (30.0, 40.0)));
        let short = turn_matrix(b, (30.0, 0.0), (70.0, 45.0), true);
        assert!(near(apply(short, (30.0, 20.0)), (50.0, 40.0)));
        assert!(!near(
            apply(
                turn_matrix(b, (30.0, 0.0), (70.0, 45.0), false),
                (30.0, 20.0)
            ),
            (50.0, 40.0)
        ));
    }
}
