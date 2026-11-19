//! The Line group, draw.io's Line section: the line's colour, opacity, width and pattern, which
//! an arrow's line and a shape's outline share, a shape's perimeter spacing, and an arrow's route
//! and heads, which change the arrows selected and nothing else.

use std::cell::Cell;
use std::rc::Rc;

use accent_drawio::{Color, Resolved};
use adw::prelude::*;

use super::{
    Cells, Change, ColourRow, Props, Read, Seen, Styles, choose, colour, combo, entry_row, owned,
    pick_shown, picture_key, show, spin,
};

/// draw.io's arrow heads (`Format.addLineMarkerItems`): the style value, whether the head is
/// filled (`startFill`/`endFill`), and a label. The circle is filled at the start and open at
/// the end, as draw.io offers it.
const HEADS: [(&str, bool, &str); 35] = [
    ("none", false, "None"),
    ("classic", true, "Classic"),
    ("classicThin", true, "Classic Thin"),
    ("open", false, "Open"),
    ("openThin", false, "Open Thin"),
    ("openAsync", false, "Open Async"),
    ("block", true, "Block"),
    ("blockThin", true, "Block Thin"),
    ("async", true, "Async"),
    ("oval", true, "Oval"),
    ("diamond", true, "Diamond"),
    ("diamondThin", true, "Diamond Thin"),
    ("classic", false, "Classic Outline"),
    ("classicThin", false, "Classic Thin Outline"),
    ("block", false, "Block Outline"),
    ("blockThin", false, "Block Thin Outline"),
    ("async", false, "Async Outline"),
    ("oval", false, "Oval Outline"),
    ("diamond", false, "Diamond Outline"),
    ("diamondThin", false, "Diamond Thin Outline"),
    ("box", false, "Box"),
    ("halfCircle", false, "Half Circle"),
    ("dash", false, "Dash"),
    ("cross", false, "Cross"),
    ("circlePlus", false, "Circle Plus"),
    ("circle", true, "Circle"),
    ("baseDash", false, "Base Dash"),
    ("ERone", false, "ER One"),
    ("ERmandOne", false, "ER Mandatory One"),
    ("ERmany", false, "ER Many"),
    ("ERoneToMany", false, "ER One to Many"),
    ("ERzeroToOne", false, "ER Zero to One"),
    ("ERzeroToMany", false, "ER Zero to Many"),
    ("doubleBlock", false, "Double Block Outline"),
    ("doubleBlock", true, "Double Block"),
];

/// Whether head `i` of [`HEADS`] is filled at the start (`start`) or the end.
fn head_filled(i: usize, start: bool) -> bool {
    let (kind, filled, _) = HEADS[i];
    filled && (start || kind != "circle")
}

/// Where in [`HEADS`] a head of `kind`, filled or not, is: the entry that matches both, else the
/// first of that kind.
fn head_at(kind: &str, filled: bool, start: bool) -> Option<usize> {
    let of_kind = |i: &usize| HEADS[*i].0 == kind;
    (0..HEADS.len())
        .filter(of_kind)
        .find(|&i| head_filled(i, start) == filled)
        .or_else(|| (0..HEADS.len()).find(of_kind))
}

/// The keys of an arrow's head at its start or its end: its kind, whether it is filled, its size.
fn head_keys(start: bool) -> [&'static str; 3] {
    match start {
        true => ["startArrow", "startFill", "startSize"],
        false => ["endArrow", "endFill", "endSize"],
    }
}

/// The heads draw.io offers a flex arrow, which draws any head as its block: none, as
/// [`HEADS`]' first writes it, and Block, writing the head alone.
const FLEX_HEADS: [&str; 2] = ["None", "Block"];

/// draw.io's line patterns (Format panel), by `dashed`, `dashPattern` and label; its Custom
/// prompt follows them in the Pattern row, starting from [`CUSTOM_PATTERN`] when the cell has
/// no pattern of its own.
const PATTERNS: [(Option<&str>, Option<&str>, &str); 8] = [
    (None, None, "Solid"),
    (Some("1"), None, "Dashed"),
    (Some("1"), Some("8 8"), "Dashed (2)"),
    (Some("1"), Some("12 12"), "Dashed (3)"),
    (Some("1"), Some("8 4 1 4"), "Dashed (4)"),
    (Some("1"), Some("1 1"), "Dotted"),
    (Some("1"), Some("1 2"), "Dotted (2)"),
    (Some("1"), Some("1 4"), "Dotted (3)"),
];
const CUSTOM_PATTERN: &str = "8 4 1 4";
/// draw.io's Waypoints menu (`Format.js`), the elbows named by its titles for them: what each
/// writes, every entry clearing `noEdgeStyle` and `libavoidRouting` and Straight keeping `curved`.
const WAYPOINTS: [(&str, Pairs); 5] = [
    ("Straight", &[("edgeStyle", None)]),
    (
        "Orthogonal",
        &[("edgeStyle", Some("orthogonalEdgeStyle")), ("curved", None)],
    ),
    (
        "Horizontal Elbow",
        &[
            ("edgeStyle", Some("elbowEdgeStyle")),
            ("elbow", Some("vertical")),
            ("curved", None),
        ],
    ),
    (
        "Vertical Elbow",
        &[
            ("edgeStyle", Some("elbowEdgeStyle")),
            ("elbow", None),
            ("curved", None),
        ],
    ),
    (
        "Curved",
        &[
            ("edgeStyle", Some("orthogonalEdgeStyle")),
            ("curved", Some("1")),
        ],
    ),
];
/// draw.io's line style menu: `rounded` and `curved`, by label.
const LINE_STYLES: [(&str, &str, &str); 3] = [
    ("0", "0", "Sharp"),
    ("1", "0", "Rounded"),
    ("0", "1", "Curved"),
];

/// Style keys and the values a menu entry gives them.
type Pairs = &'static [(&'static str, Option<&'static str>)];

/// The Line group and its rows.
pub(super) struct Line {
    pub(super) group: adw::PreferencesGroup,
    stroke: Rc<ColourRow>,
    /// The line's own opacity (`strokeOpacity`), which leaves the fill and the label as they are.
    opacity: adw::SpinRow,
    stroke_width: adw::SpinRow,
    pattern: adw::ComboRow,
    /// The numbers of a pattern draw.io's menu does not list, its Custom prompt.
    dash_pattern: adw::EntryRow,
    /// A shape's `perimeterSpacing`, how far off its outline arrows stop.
    perimeter: adw::SpinRow,
    waypoints: adw::ComboRow,
    line_style: adw::ComboRow,
    start: adw::ComboRow,
    end: adw::ComboRow,
    start_size: adw::SpinRow,
    end_size: adw::SpinRow,
    /// `sourcePerimeterSpacing` and `targetPerimeterSpacing`.
    start_spacing: adw::SpinRow,
    end_spacing: adw::SpinRow,
    /// A flex arrow's band and its heads' widths (`width`, `startWidth`, `endWidth`).
    arrow_width: adw::SpinRow,
    start_width: adw::SpinRow,
    end_width: adw::SpinRow,
    /// Whether Start and End offer a flex arrow's heads ([`FLEX_HEADS`]) rather than [`HEADS`].
    flex: Cell<bool>,
}

impl Line {
    /// Its rows in `group`: the line's own, then an arrow's.
    pub(super) fn new(group: adw::PreferencesGroup) -> Line {
        let stroke = Rc::new(ColourRow::new("Line Colour", Color::BLACK));
        let opacity = spin("Line Opacity", (0.0, 100.0, 5.0));
        let stroke_width = spin("Line Width", (0.0, 20.0, 0.5));
        stroke_width.set_digits(1);
        let pattern = combo(
            "Pattern",
            &[&PATTERNS.map(|(.., l)| l)[..], &["Custom"]].concat(),
        );
        let dash_pattern = entry_row("Dash Pattern");
        let perimeter = spin("Perimeter", (0.0, 999.0, 1.0));
        group.add(&stroke.row);
        group.add(&opacity);
        group.add(&stroke_width);
        group.add(&pattern);
        group.add(&dash_pattern);
        group.add(&perimeter);

        let waypoints = combo("Waypoints", &WAYPOINTS.map(|(l, _)| l));
        let line_style = combo("Line Style", &LINE_STYLES.map(|(.., l)| l));
        let start = combo("Start", &HEADS.map(|(.., l)| l));
        let end = combo("End", &HEADS.map(|(.., l)| l));
        let start_size = spin("Start Size", (0.0, 999.0, 1.0));
        let end_size = spin("End Size", (0.0, 999.0, 1.0));
        let start_spacing = spin("Start Spacing", (-999.0, 999.0, 1.0));
        let end_spacing = spin("End Spacing", (-999.0, 999.0, 1.0));
        let arrow_width = spin("Arrow Width", (1.0, 999.0, 1.0));
        let start_width = spin("Start Width", (0.0, 999.0, 1.0));
        let end_width = spin("End Width", (0.0, 999.0, 1.0));
        group.add(&waypoints);
        group.add(&line_style);
        group.add(&arrow_width);
        group.add(&start);
        group.add(&start_size);
        group.add(&start_width);
        group.add(&start_spacing);
        group.add(&end);
        group.add(&end_size);
        group.add(&end_width);
        group.add(&end_spacing);
        Line {
            group,
            stroke,
            opacity,
            stroke_width,
            pattern,
            dash_pattern,
            perimeter,
            waypoints,
            line_style,
            start,
            end,
            start_size,
            end_size,
            start_spacing,
            end_spacing,
            arrow_width,
            start_width,
            end_width,
            flex: Cell::new(false),
        }
    }

    /// Send each row's change; `send` drops what the pane's own filling sets.
    pub(super) fn wire(&self, props: &Rc<Props>, send: Rc<dyn Fn(Change)>) {
        colour(
            &self.stroke,
            &props.filling,
            &send,
            Cells::All,
            "strokeColor",
        );
        // The last ones are an arrow's alone, at mxGraph's defaults: 6 a head's size
        // (`mxConstants.DEFAULT_MARKERSIZE`), 10 a flex arrow's band and 20 its heads' width.
        props.spins(
            &send,
            &[
                (&self.opacity, "strokeOpacity", Some(100.0), Cells::All),
                (&self.stroke_width, "strokeWidth", None, Cells::All),
                (
                    &self.perimeter,
                    "perimeterSpacing",
                    Some(0.0),
                    Cells::Shapes,
                ),
                (&self.start_size, "startSize", Some(6.0), Cells::Arrows),
                (&self.end_size, "endSize", Some(6.0), Cells::Arrows),
                (
                    &self.start_spacing,
                    "sourcePerimeterSpacing",
                    Some(0.0),
                    Cells::Arrows,
                ),
                (
                    &self.end_spacing,
                    "targetPerimeterSpacing",
                    Some(0.0),
                    Cells::Arrows,
                ),
                (&self.arrow_width, "width", Some(10.0), Cells::Arrows),
                (&self.start_width, "startWidth", Some(20.0), Cells::Arrows),
                (&self.end_width, "endWidth", Some(20.0), Cells::Arrows),
            ],
        );
        choose(
            &self.pattern,
            &send,
            Box::new(|i| {
                let (dashed, pattern, _) = PATTERNS.get(i)?;
                let pairs = [("dashed", *dashed), ("dashPattern", *pattern)];
                Some(Change::Style(Cells::All, owned(&pairs)))
            }),
        );
        // Custom writes nothing until its numbers are applied, as draw.io's prompt.
        let me = Rc::downgrade(props);
        self.pattern.connect_selected_notify(move |row| {
            let Some(me) = me.upgrade() else { return };
            let custom = row.selected() as usize == PATTERNS.len();
            me.line.dash_pattern.set_visible(custom);
            if custom && !me.filling.get() {
                me.line.dash_pattern.grab_focus();
            }
        });
        let dash_send = send.clone();
        props.typed(&self.dash_pattern, "dash pattern", move |text| {
            let numbers = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let pattern = (!numbers.is_empty()).then_some(numbers);
            dash_send(Change::Style(
                Cells::All,
                vec![("dashed", Some("1".to_string())), ("dashPattern", pattern)],
            ))
        });
        for (row, start) in [(&self.start, true), (&self.end, false)] {
            let [arrow, fill, _] = head_keys(start);
            let me = Rc::downgrade(props);
            choose(
                row,
                &send,
                Box::new(move |i| {
                    let pairs = match (me.upgrade()?.line.flex.get(), i) {
                        (true, 1) => owned(&[(arrow, Some("block"))]),
                        (true, 2..) => return None,
                        _ => {
                            let (kind, ..) = HEADS.get(i)?;
                            let filled = if head_filled(i, start) { "1" } else { "0" };
                            owned(&[(arrow, Some(*kind)), (fill, Some(filled))])
                        }
                    };
                    Some(Change::Style(Cells::Arrows, pairs))
                }),
            );
        }
        choose(
            &self.waypoints,
            &send,
            Box::new(|i| {
                let (_, pairs) = WAYPOINTS.get(i)?;
                let clear = [("noEdgeStyle", None), ("libavoidRouting", None)];
                let pairs = owned(&[*pairs, &clear].concat());
                Some(Change::Style(Cells::Arrows, pairs))
            }),
        );
        choose(
            &self.line_style,
            &send,
            Box::new(|i| {
                let (rounded, curved, _) = LINE_STYLES.get(i)?;
                let pairs = [("rounded", Some(*rounded)), ("curved", Some(*curved))];
                Some(Change::Style(Cells::Arrows, owned(&pairs)))
            }),
        );
    }

    /// Show each row over the cells it reads in `styles`, the route and the heads where `edges`
    /// are selected and the perimeter where `vertices` are alone, the first cell's `style`
    /// deciding whether the heads are a flex arrow's.
    pub(super) fn fill(&self, style: &Resolved, styles: &Styles, vertices: bool, edges: bool) {
        self.perimeter.set_visible(vertices && !edges);
        let arrow: [&gtk::Widget; 8] = [
            self.waypoints.upcast_ref(),
            self.line_style.upcast_ref(),
            self.start.upcast_ref(),
            self.start_size.upcast_ref(),
            self.start_spacing.upcast_ref(),
            self.end.upcast_ref(),
            self.end_size.upcast_ref(),
            self.end_spacing.upcast_ref(),
        ];
        for row in arrow {
            row.set_visible(edges);
        }
        // The first cell decides, as draw.io's `ss.style` does.
        let flex = edges && style.shape() == "flexArrow";
        for row in [&self.arrow_width, &self.start_width, &self.end_width] {
            row.set_visible(flex);
        }
        if self.flex.replace(flex) != flex {
            // A list each, since a row marks its own Mixed in it.
            for row in [&self.start, &self.end] {
                let heads = match flex {
                    true => gtk::StringList::new(&FLEX_HEADS),
                    false => gtk::StringList::new(&HEADS.map(|(.., l)| l)),
                };
                row.set_model(Some(&heads));
            }
        }
        let flex_arrow = |c: &Seen| c.edge && c.style.shape() == "flexArrow";
        let widths = [
            (&self.arrow_width, "width", 10.0),
            (&self.start_width, "startWidth", 20.0),
            (&self.end_width, "endWidth", 20.0),
        ];
        for (row, key, default) in widths {
            show(
                row,
                styles.read_where(|c| flex_arrow(c).then(|| c.style.num(key, default))),
            );
        }
        let read = |cells, key, default| styles.read(cells, |s| s.num(key, default));
        show(
            &self.perimeter,
            read(Cells::Shapes, "perimeterSpacing", 0.0),
        );
        let spacing = read(Cells::Arrows, "sourcePerimeterSpacing", 0.0);
        show(&self.start_spacing, spacing);
        let spacing = read(Cells::Arrows, "targetPerimeterSpacing", 0.0);
        show(&self.end_spacing, spacing);
        self.stroke
            .fill(styles.read(Cells::All, |s| s.color(picture_key(s, "strokeColor"))));
        show(&self.opacity, read(Cells::All, "strokeOpacity", 100.0));
        show(&self.stroke_width, read(Cells::All, "strokeWidth", 1.0));
        // A pattern draw.io's menu does not list is Custom, its numbers in the row below.
        let pattern = styles.read(Cells::All, pattern_of);
        let (at, numbers) = &pattern.value;
        self.dash_pattern
            .set_text(numbers.as_deref().unwrap_or(CUSTOM_PATTERN));
        let picked = Read {
            value: Some(*at),
            mixed: pattern.mixed,
        };
        pick_shown(&self.pattern, picked);
        self.dash_pattern
            .set_visible(!pattern.mixed && *at == PATTERNS.len());
        let heads = [
            (&self.start, &self.start_size, true),
            (&self.end, &self.end_size, false),
        ];
        for (row, size, start) in heads {
            pick_shown(row, styles.read(Cells::Arrows, |s| head_of(s, start, flex)));
            show(size, read(Cells::Arrows, head_keys(start)[2], 6.0));
        }
        pick_shown(&self.waypoints, styles.read(Cells::Arrows, route_of));
        let corners = styles.read(Cells::Arrows, |s| Some(corners_of(s)));
        pick_shown(&self.line_style, corners);
    }
}

/// Where in the Pattern row `style`'s line is, and its numbers: Solid, an entry of draw.io's
/// menu, or Custom for any other.
fn pattern_of(style: &Resolved) -> (usize, Option<String>) {
    let numbers = style
        .get("dashPattern")
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "));
    let at = match style.flag("dashed", false) {
        false => 0,
        true => (1..PATTERNS.len())
            .find(|&i| PATTERNS[i].1.map(str::to_string) == numbers)
            .unwrap_or(PATTERNS.len()),
    };
    (at, numbers)
}

/// Where in the Start (else End) row `style`'s head is, among a flex arrow's heads with `flex`.
fn head_of(style: &Resolved, start: bool, flex: bool) -> Option<usize> {
    let [arrow, fill, _] = head_keys(start);
    let kind = style.get(arrow).unwrap_or("none");
    match flex {
        true => Some(usize::from(kind != "none")),
        false => head_at(kind, style.flag(fill, true), start),
    }
}

/// Where in the Waypoints row `style`'s route is, none for an edge style draw.io's menu does
/// not offer.
fn route_of(style: &Resolved) -> Option<usize> {
    let curved = style.flag("curved", false);
    match (style.get("edgeStyle"), style.get("elbow"), curved) {
        (None, ..) => Some(0),
        (Some("orthogonalEdgeStyle"), _, false) => Some(1),
        (Some("elbowEdgeStyle"), Some("vertical"), _) => Some(2),
        (Some("elbowEdgeStyle"), ..) => Some(3),
        (Some("orthogonalEdgeStyle"), _, true) => Some(4),
        _ => None,
    }
}

/// Where in the Line Style row `style`'s corners are: sharp, rounded or curved.
fn corners_of(style: &Resolved) -> usize {
    match (style.flag("curved", false), style.flag("rounded", false)) {
        (true, _) => 2,
        (false, rounded) => usize::from(rounded),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Cells, Seen, Styles};

    /// Two arrows read alike where their patterns are one entry of the menu, written apart, and
    /// mixed where their heads differ.
    #[test]
    fn rows_read_what_each_cell_shows() {
        let seen = |style: &str| Seen {
            style: accent_drawio::Style::parse(style).resolve(true),
            vertex: false,
            edge: true,
        };
        let styles = Styles(vec![
            seen("dashed=1;dashPattern=8 8;endArrow=block;"),
            seen("dashed=1;dashPattern= 8  8 ;endArrow=open;"),
        ]);
        let pattern = styles.read(Cells::All, super::pattern_of);
        assert_eq!((pattern.value.0, pattern.mixed), (2, false));
        let heads = styles.read(Cells::Arrows, |s| super::head_of(s, false, false));
        assert!(heads.mixed);
        let shapes = styles.read(Cells::Shapes, |s| s.num("arcSize", 15.0));
        assert_eq!((shapes.value, shapes.mixed), (0.0, false));
    }
}
