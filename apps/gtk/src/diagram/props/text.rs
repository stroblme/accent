//! The Text group: a label's font, size, colour and style, its alignment, a shape's label
//! placed as draw.io's menu places it, word wrap and draw.io's Vertical, and the label's
//! background, border, opacity and spacing.

use std::rc::Rc;

use accent_drawio::{Color, Resolved};
use adw::prelude::*;

use super::{
    Cells, Change, Check, ColourRow, MIXED, Props, Styles, choose, colour, combo, owned, pick,
    pick_own, pick_shown, show, spin, style,
};

/// draw.io's fonts (`Menus.defaultFonts`).
const FONTS: [&str; 9] = [
    "Helvetica",
    "Verdana",
    "Times New Roman",
    "Garamond",
    "Comic Sans MS",
    "Courier New",
    "Georgia",
    "Lucida Console",
    "Tahoma",
];
/// draw.io's label positions (`Format.js`): `labelPosition`, `verticalLabelPosition`, `align`
/// and `verticalAlign`, by label; a label beside its shape is aligned towards it.
const POSITIONS: [(&str, [&str; 4]); 9] = [
    ("Top Left", ["left", "top", "right", "bottom"]),
    ("Top", ["center", "top", "center", "bottom"]),
    ("Top Right", ["right", "top", "left", "bottom"]),
    ("Left", ["left", "middle", "right", "middle"]),
    ("Centre", ["center", "middle", "center", "middle"]),
    ("Right", ["right", "middle", "left", "middle"]),
    ("Bottom Left", ["left", "bottom", "right", "top"]),
    ("Bottom", ["center", "bottom", "center", "top"]),
    ("Bottom Right", ["right", "bottom", "left", "top"]),
];
const ALIGN: [(&str, &str); 3] = [("left", "Left"), ("center", "Centre"), ("right", "Right")];
const VALIGN: [(&str, &str); 3] = [("top", "Top"), ("middle", "Middle"), ("bottom", "Bottom")];

/// The Text group and its rows.
pub(super) struct Text {
    pub(super) group: adw::PreferencesGroup,
    font: adw::ComboRow,
    font_size: adw::SpinRow,
    font_colour: Rc<ColourRow>,
    bold: gtk::ToggleButton,
    italic: gtk::ToggleButton,
    underline: gtk::ToggleButton,
    /// draw.io's Vertical: the label across the height (`horizontal=0`).
    vertical: Check,
    align: adw::ComboRow,
    valign: adw::ComboRow,
    label_position: adw::ComboRow,
    wrap: Check,
    label_background: Rc<ColourRow>,
    label_border: Rc<ColourRow>,
    text_opacity: adw::SpinRow,
    /// `spacing` and its four sides, draw.io's Text Spacing.
    spacing: [adw::SpinRow; 5],
}

impl Text {
    /// Its rows, in `group`.
    pub(super) fn new(group: adw::PreferencesGroup) -> Text {
        let font = combo("Font", &FONTS);
        let font_size = spin("Font Size", (6.0, 96.0, 1.0));
        let font_colour = Rc::new(ColourRow::new("Font Colour", Color::BLACK));
        let toggle = |icon: &str, tooltip: &str| {
            gtk::ToggleButton::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .valign(gtk::Align::Center)
                .build()
        };
        let bold = toggle("format-text-bold-symbolic", "Bold");
        let italic = toggle("format-text-italic-symbolic", "Italic");
        let underline = toggle("format-text-underline-symbolic", "Underline");
        let styles = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        styles.add_css_class("linked");
        for b in [&bold, &italic, &underline] {
            styles.append(b);
        }
        let style_row = adw::ActionRow::builder().title("Style").build();
        style_row.add_suffix(&styles);
        let vertical = Check::new("Vertical Text");
        let align = combo("Alignment", &ALIGN.map(|(_, l)| l));
        let valign = combo("Vertical Alignment", &VALIGN.map(|(_, l)| l));
        let label_position = combo("Position", &POSITIONS.map(|(l, _)| l));
        let wrap = Check::new("Word Wrap");
        let label_background = Rc::new(ColourRow::new("Label Background", Color::WHITE));
        let label_border = Rc::new(ColourRow::new("Label Border", Color::BLACK));
        let text_opacity = spin("Text Opacity", (0.0, 100.0, 5.0));
        let spacing = ["Global", "Top", "Left", "Bottom", "Right"]
            .map(|side| spin(side, (-999.0, 999.0, 1.0)));
        let spacing_row = adw::ExpanderRow::builder().title("Spacing").build();
        for row in &spacing {
            spacing_row.add_row(row);
        }
        group.add(&font);
        group.add(&font_size);
        group.add(&font_colour.row);
        group.add(&style_row);
        group.add(&vertical.row);
        group.add(&align);
        group.add(&valign);
        group.add(&label_position);
        group.add(&wrap.row);
        group.add(&label_background.row);
        group.add(&label_border.row);
        group.add(&text_opacity);
        group.add(&spacing_row);
        Text {
            group,
            font,
            font_size,
            font_colour,
            bold,
            italic,
            underline,
            vertical,
            align,
            valign,
            label_position,
            wrap,
            label_background,
            label_border,
            text_opacity,
            spacing,
        }
    }

    /// Send each row's change; `send` drops what the pane's own filling sets.
    pub(super) fn wire(&self, props: &Rc<Props>, send: Rc<dyn Fn(Change)>) {
        for (row, key) in [
            (&self.font_colour, "fontColor"),
            (&self.label_background, "labelBackgroundColor"),
            (&self.label_border, "labelBorderColor"),
        ] {
            colour(row, &props.filling, &send, Cells::All, key);
        }
        let set = style(&send, Cells::Shapes, "whiteSpace");
        self.wrap
            .connect(move |on| set(on.then(|| "wrap".to_string())));
        // draw.io's Vertical toggles `horizontal`, writing 1 when it goes off.
        let set = style(&send, Cells::Shapes, "horizontal");
        self.vertical
            .connect(move |on| set(Some(if on { "0" } else { "1" }.into())));
        let [global, top, left, bottom, right] = &self.spacing;
        props.spins(
            &send,
            &[
                (&self.font_size, "fontSize", None, Cells::All),
                (&self.text_opacity, "textOpacity", Some(100.0), Cells::All),
                (global, "spacing", None, Cells::All),
                (top, "spacingTop", None, Cells::All),
                (left, "spacingLeft", None, Cells::All),
                (bottom, "spacingBottom", None, Cells::All),
                (right, "spacingRight", None, Cells::All),
            ],
        );
        // Each cell keeps its other bits, as draw.io's Bold does (`mxGraph.setCellStyleFlags`).
        for (button, bit) in [(&self.bold, 1), (&self.italic, 2), (&self.underline, 4)] {
            let send = send.clone();
            button.connect_toggled(move |button| {
                button.unset_state_flags(gtk::StateFlags::INCONSISTENT);
                send(Change::FontStyle(bit, button.is_active()));
            });
        }
        // Each entry writes its name, the first cell's own font beyond draw.io's list too, so
        // picking that gives it to every cell selected; Mixed writes nothing.
        let set = style(&send, Cells::All, "fontFamily");
        self.font.connect_selected_notify(move |row| {
            let font = row.selected_item().and_downcast::<gtk::StringObject>();
            if let Some(font) = font.map(|f| f.string()).filter(|f| f.as_str() != MIXED) {
                set(Some(font.to_string()));
            }
        });
        let align = ALIGN.map(|(v, _)| v).to_vec();
        pick(&self.align, &send, Cells::All, "align", align);
        let valign = VALIGN.map(|(v, _)| v).to_vec();
        pick(&self.valign, &send, Cells::All, "verticalAlign", valign);
        choose(
            &self.label_position,
            &send,
            Box::new(|i| {
                let (_, values) = POSITIONS.get(i)?;
                let keys = [
                    "labelPosition",
                    "verticalLabelPosition",
                    "align",
                    "verticalAlign",
                ];
                let pairs: Vec<_> = keys.into_iter().zip(values.map(Some)).collect();
                Some(Change::Style(Cells::Shapes, owned(&pairs)))
            }),
        );
    }

    /// Show each row over the cells it reads in `styles`, and a shape's own rows where
    /// `vertices` are selected.
    pub(super) fn fill(&self, styles: &Styles, vertices: bool) {
        // An arrow's label sits on its line, at no size to wrap to or turn across.
        self.label_position.set_visible(vertices);
        self.wrap.row.set_visible(vertices);
        self.vertical.row.set_visible(vertices);
        let read = |key, default| styles.read(Cells::All, |s| s.num(key, default));
        show(&self.font_size, read("fontSize", 12.0));
        let colours = [
            (&self.font_colour, "fontColor"),
            (&self.label_background, "labelBackgroundColor"),
            (&self.label_border, "labelBorderColor"),
        ];
        for (row, key) in colours {
            row.fill(styles.read(Cells::All, |s| s.color(key)));
        }
        show(&self.text_opacity, read("textOpacity", 100.0));
        let sides = [
            "spacing",
            "spacingTop",
            "spacingLeft",
            "spacingBottom",
            "spacingRight",
        ];
        for (row, key) in self.spacing.iter().zip(sides) {
            show(row, read(key, if key == "spacing" { 2.0 } else { 0.0 }));
        }
        self.vertical
            .fill(styles.read(Cells::Shapes, |s| !s.flag("horizontal", true)));
        self.wrap
            .fill(styles.read(Cells::Shapes, |s| s.get("whiteSpace") == Some("wrap")));
        // A bit some cells have and others not shows the toggle out with the hatch Mixed has,
        // and a click sets it on them all.
        for (button, bit) in [(&self.bold, 1), (&self.italic, 2), (&self.underline, 4)] {
            let on = styles.read(Cells::All, |s| s.num("fontStyle", 0.0) as u32 & bit != 0);
            button.set_active(on.value && !on.mixed);
            match on.mixed {
                true => button.set_state_flags(gtk::StateFlags::INCONSISTENT, false),
                false => button.unset_state_flags(gtk::StateFlags::INCONSISTENT),
            }
            let pressed = match (on.mixed, on.value) {
                (true, _) => gtk::AccessibleTristate::Mixed,
                (false, true) => gtk::AccessibleTristate::True,
                (false, false) => gtk::AccessibleTristate::False,
            };
            button.update_state(&[gtk::accessible::State::Pressed(pressed)]);
        }
        // A font draw.io's list does not have is an entry of its own, the first cell's where
        // they differ.
        let font = styles.read(Cells::All, |s| {
            let family = s.get("fontFamily").unwrap_or(FONTS[0]);
            (FONTS.iter().position(|f| *f == family), family.to_string())
        });
        pick_own(&self.font, FONTS.len(), font);
        // Centred and in the middle where the style says nothing, as mxGraph aligns a label.
        let at = |values: &[&str], v: &str| values.iter().position(|x| *x == v);
        let aligns = ALIGN.map(|(v, _)| v);
        pick_shown(
            &self.align,
            styles.read(Cells::All, |s| {
                at(&aligns, s.get("align").unwrap_or("center"))
            }),
        );
        let valigns = VALIGN.map(|(v, _)| v);
        let valign = |s: &Resolved| at(&valigns, s.get("verticalAlign").unwrap_or("middle"));
        pick_shown(&self.valign, styles.read(Cells::All, valign));
        pick_shown(
            &self.label_position,
            styles.read(Cells::Shapes, |s| Some(position_of(s))),
        );
    }
}

/// Where in the Position row `style`'s label is, as draw.io reads it: a side named with no
/// corner is its middle.
fn position_of(style: &Resolved) -> usize {
    let side = style.get("labelPosition").unwrap_or("center");
    let height = style.get("verticalLabelPosition").unwrap_or("middle");
    let placed = |height: &str| {
        POSITIONS
            .iter()
            .position(|(_, [s, h, ..])| *s == side && *h == height)
    };
    placed(height).or_else(|| placed("middle")).unwrap_or(4)
}
