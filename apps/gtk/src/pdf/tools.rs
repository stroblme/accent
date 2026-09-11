//! The drawing tools' pure geometry: what a drag means under each of them, and what the Adjust
//! tool takes hold of.

/// What a drag over the page does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Select the text under it, which is what a drag has always done.
    #[default]
    Select,
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
    /// Take hold of a stroke: drag its middle to move it, an edge to stretch it, a corner to
    /// scale it.
    Adjust,
}

impl Mode {
    /// Whether a drag draws — everything but selecting, erasing and adjusting.
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

    /// The window action that puts this tool in hand, or `None` for Select, which is what having
    /// no tool in hand is. The one map from a tool to a name: the ring's buttons, the status
    /// bar's readout and the palette all read the label out of `ACTIONS` through it, so the three
    /// cannot drift apart.
    pub fn action(self) -> Option<&'static str> {
        Some(match self {
            Mode::Select => return None,
            Mode::Pen => "win.pdf-pen",
            Mode::Highlighter => "win.pdf-highlighter",
            Mode::Eraser => "win.pdf-eraser",
            Mode::Line => "win.pdf-line",
            Mode::Rect => "win.pdf-rect",
            Mode::Circle => "win.pdf-circle",
            Mode::Adjust => "win.pdf-adjust",
        })
    }

    /// Whose width and colour a tool draws in: its own for the highlighter and the eraser, the
    /// pen's for the pen and every shape.
    pub fn style_owner(self) -> Mode {
        match self {
            Mode::Highlighter | Mode::Eraser => self,
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
        (Mode::Adjust, "tool-adjust-symbolic"),
    ];

    fn action(self) -> Option<&'static str> {
        Mode::action(self)
    }

    fn none() -> Mode {
        Mode::Select
    }
}

/// How close to an axis, in degrees, a line has to be to snap onto it.
const SNAP_DEG: f32 = 7.0;
/// How near the pointer has to come to a stroke, in page points, for the Adjust tool to take it.
pub(super) const ADJUST_RADIUS: f32 = 4.0;
/// The side of an Adjust handle, in pixels.
pub(super) const HANDLE: f32 = 8.0;

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

/// Where on a selected stroke's box the Adjust tool took hold.
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
    }
}

/// A box under a map, normalised. Exact for the axis-aligned maps a drag makes.
pub(super) fn mapped(
    r: accent_core::pdf::Rect,
    m: accent_core::pdf::Matrix,
) -> accent_core::pdf::Rect {
    accent_core::pdf::Rect::from_corners(
        accent_core::pdf::apply(m, (r.left, r.top)),
        accent_core::pdf::apply(m, (r.right, r.bottom)),
    )
}

/// The stroke the Adjust tool has hold of, and the drag being applied to it.
pub struct Selected {
    pub page: usize,
    /// The render thread's name for it, which follows it through a move.
    pub id: u32,
    pub points: Vec<(f32, f32)>,
    pub bounds: accent_core::pdf::Rect,
    pub style: accent_core::pdf::InkStyle,
    /// Where the hand is holding it, while it is.
    pub handle: Option<Handle>,
    /// What the drag so far amounts to, painted over the page until the hand lets go.
    pub matrix: accent_core::pdf::Matrix,
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
    fn a_line_snaps_to_the_axis_within_seven_degrees() {
        assert_eq!(snap((0.0, 0.0), (100.0, 10.0)), (100.0, 0.0));
        assert_eq!(snap((0.0, 0.0), (100.0, 20.0)), (100.0, 20.0));
        assert_eq!(snap((0.0, 0.0), (5.0, 100.0)), (0.0, 100.0));
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
}
