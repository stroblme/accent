//! Focus mode's line fade: at High the text away from the caret recedes, further the further it
//! is, the way Apostrophe's focus mode leaves only the sentence being written at full strength.
//!
//! A veil rather than text tags. `GtkTextTag` has no opacity, a `foreground-rgba` would flatten a
//! note's link colours and a code tab's syntax colours into one grey, and re-tagging the buffer
//! each time the fade comes and goes is the tag churn ROADMAP measures at 10–21 ms on a long note.
//! So the view paints a band of its own background over each visible line, above the text, and
//! the buffer is never touched: the cost is one rectangle per line on screen, whatever the note's
//! length.

use crate::multicaret;
use gtk::prelude::*;
use gtk::{gdk, graphene};
use sourceview5::prelude::*;
use std::ops::RangeInclusive;

/// How far the fade reaches, in buffer lines: a paragraph in prose, a line in code.
const SIGMA: f32 = 2.0;
/// What is left of a line far from the caret, and of a pane that is not the one being written in
/// (`.chrome-away`, build.rs).
pub const FLOOR: f32 = 0.3;
/// How long the fade takes to come in or go, which is the chrome's own transition (build.rs).
pub const RAMP_MS: u32 = 250;

/// How much of a line `d` buffer lines from the caret shows: all of it at the caret, down a
/// Gaussian to [`FLOOR`].
pub fn alpha(d: i32) -> f32 {
    let d = d as f32;
    FLOOR + (1.0 - FLOOR) * (-(d * d) / (2.0 * SIGMA * SIGMA)).exp()
}

/// How many lines `line` is from `span`; nothing inside it.
pub fn distance(line: i32, span: &RangeInclusive<i32>) -> i32 {
    (span.start() - line).max(line - span.end()).max(0)
}

/// Veil every line of `view` on screen by its distance from the carets, `strength` of the way in.
/// Called from the view's `AboveText` layer, which draws in buffer coordinates.
///
/// Each band is as wide as the text window, so it covers the page gutters and the heading
/// markers that hang in the left one, and stops at the line-number gutter, which is a window of
/// its own.
pub fn paint(view: &multicaret::View, snapshot: &gtk::Snapshot, strength: f32) {
    let span = span(view);
    let veil = veil(view.upcast_ref());
    let visible = view.visible_rect();
    let bottom = visible.y() + visible.height();
    let (mut line, _) = view.line_at_y(visible.y());
    let found = found(view, line, bottom);
    loop {
        let (y, height) = view.line_yrange(&line);
        if y >= bottom {
            break;
        }
        let cover = cover(line.line(), &span, &found) * strength;
        // A folded line has no height, and a line at the caret nothing to cover.
        if height > 0 && cover > 0.0 {
            snapshot.append_color(
                &crate::theme::at(veil, veil.alpha() * cover),
                &graphene::Rect::new(
                    visible.x() as f32,
                    y as f32,
                    visible.width() as f32,
                    height as f32,
                ),
            );
        }
        if !line.forward_line() {
            break;
        }
    }
}

/// How much of a line the veil takes at full strength: what [`alpha`] leaves of it, and nothing
/// of a line holding a find-bar match, so a search left open stays readable while the rest of
/// the text recedes.
fn cover(line: i32, span: &RangeInclusive<i32>, found: &[RangeInclusive<i32>]) -> f32 {
    if found.iter().any(|lines| lines.contains(&line)) {
        return 0.0;
    }
    1.0 - alpha(distance(line, span))
}

/// The lines from `at` down to `bottom` holding a match of the find bar's query, first to
/// last, or none while its highlight is off. One match is all a line needs, so the walk goes on
/// from the line after each, which also keeps a match of no width from being found forever.
fn found(view: &multicaret::View, mut at: gtk::TextIter, bottom: i32) -> Vec<RangeInclusive<i32>> {
    let Some(search) = view.highlighted_search() else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    while let Some((start, mut end, wrapped)) = search.forward(&at) {
        if wrapped || view.line_yrange(&start).0 >= bottom {
            break;
        }
        lines.push(start.line()..=end.line());
        if !end.forward_line() {
            break;
        }
        at = end;
    }
    lines
}

/// The lines the carets and their selections cover, first to last: every caret *and* every
/// anchor, so a selection made upwards at a column does not run into veiled lines.
pub(crate) fn span(view: &multicaret::View) -> RangeInclusive<i32> {
    let selections = view.selections();
    let lines = || {
        selections
            .iter()
            .flat_map(|(from, to)| [from.line(), to.line()])
    };
    lines().min().unwrap_or(0)..=lines().max().unwrap_or(0)
}

/// What the view's background is painted in, which is what a veiled line recedes into: the
/// theme's view background for a note (`textview.accent-doc`, build.rs), the style scheme's own
/// for code and CSV.
fn veil(view: &sourceview5::View) -> gdk::RGBA {
    let dark = adw::StyleManager::default().is_dark();
    let page = gdk::RGBA::parse(crate::theme::view_bg(dark)).unwrap_or(gdk::RGBA::TRANSPARENT);
    if view.has_css_class("accent-doc") {
        return page;
    }
    view.buffer()
        .downcast::<sourceview5::Buffer>()
        .ok()
        .and_then(|buffer| buffer.style_scheme())
        .and_then(|scheme| scheme.style("text"))
        .and_then(|style| style.background())
        .and_then(|colour| gdk::RGBA::parse(&colour).ok())
        .unwrap_or(page)
}

#[cfg(test)]
mod tests {
    use super::{FLOOR, SIGMA, alpha, cover, distance};

    /// The caret's own line is untouched, nothing further away is brighter than something nearer,
    /// and from three sigmas out a line has settled on what a far line keeps.
    #[test]
    fn a_line_fades_down_a_gaussian_to_the_floor() {
        assert_eq!(alpha(0), 1.0);
        for d in 0..20 {
            assert!(alpha(d + 1) <= alpha(d), "brighter at {}", d + 1);
        }
        for d in (3.0 * SIGMA) as i32..40 {
            assert!((alpha(d) - FLOOR).abs() < 0.01, "not settled at {d}");
        }
    }

    /// Every line the carets and the selection cover is the caret's line.
    #[test]
    fn the_distance_is_counted_from_the_edge_of_the_caret_span() {
        assert_eq!(distance(5, &(3..=7)), 0);
        assert_eq!(distance(3, &(3..=7)), 0);
        assert_eq!(distance(7, &(3..=7)), 0);
        assert_eq!(distance(1, &(3..=7)), 2);
        assert_eq!(distance(10, &(3..=7)), 3);
        assert_eq!(distance(4, &(4..=4)), 0);
    }

    /// A line holding a find-bar match keeps all of itself however far it is from the caret,
    /// and the lines around it fade as they would without it.
    #[test]
    fn a_line_holding_a_match_is_not_veiled() {
        let found = [10..=11];
        assert_eq!(cover(10, &(0..=0), &found), 0.0);
        assert_eq!(cover(11, &(0..=0), &found), 0.0);
        assert_eq!(cover(12, &(0..=0), &found), cover(12, &(0..=0), &[]));
        assert!(cover(12, &(0..=0), &found) > 0.6);
    }
}
