//! The Text group: a label's font, size, colour and style, its alignment, a shape's label
//! placed as draw.io's menu places it, word wrap and draw.io's Vertical, and the label's
//! background, border, opacity and spacing.

use std::cell::Cell;
use std::rc::Rc;

use accent_drawio::{Color, Resolved};
use adw::prelude::*;

use super::{Change, ColourRow, Props, choose, colour, combo, found, owned, pick, spin, style};

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
    vertical: adw::SwitchRow,
    align: adw::ComboRow,
    valign: adw::ComboRow,
    label_position: adw::ComboRow,
    wrap: adw::SwitchRow,
    label_background: Rc<ColourRow>,
    label_border: Rc<ColourRow>,
    text_opacity: adw::SpinRow,
    /// `spacing` and its four sides, draw.io's Text Spacing.
    spacing: [adw::SpinRow; 5],
    /// The font style bits the cell had when the pane was filled, which a toggle flips one of.
    font_bits: Cell<u32>,
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
        let vertical = adw::SwitchRow::builder().title("Vertical Text").build();
        let align = combo("Alignment", &ALIGN.map(|(_, l)| l));
        let valign = combo("Vertical Alignment", &VALIGN.map(|(_, l)| l));
        let label_position = combo("Position", &POSITIONS.map(|(l, _)| l));
        let wrap = adw::SwitchRow::builder().title("Word Wrap").build();
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
        group.add(&vertical);
        group.add(&align);
        group.add(&valign);
        group.add(&label_position);
        group.add(&wrap);
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
            font_bits: Cell::new(0),
        }
    }

    /// Send each row's change; `send` drops what the pane's own filling sets.
    pub(super) fn wire(&self, props: &Rc<Props>, send: Rc<dyn Fn(Change)>) {
        colour(&self.font_colour, &props.filling, &send, "fontColor");
        colour(
            &self.label_background,
            &props.filling,
            &send,
            "labelBackgroundColor",
        );
        colour(
            &self.label_border,
            &props.filling,
            &send,
            "labelBorderColor",
        );
        let set = style(&send, "whiteSpace");
        self.wrap
            .connect_active_notify(move |row| set(row.is_active().then(|| "wrap".to_string())));
        // draw.io's Vertical toggles `horizontal`, writing 1 when it goes off.
        let set = style(&send, "horizontal");
        self.vertical.connect_active_notify(move |row| {
            set(Some(if row.is_active() { "0" } else { "1" }.into()))
        });
        let [global, top, left, bottom, right] = &self.spacing;
        props.spins(
            &send,
            &[
                (&self.font_size, "fontSize", None, false),
                (&self.text_opacity, "textOpacity", Some(100.0), false),
                (global, "spacing", None, false),
                (top, "spacingTop", None, false),
                (left, "spacingLeft", None, false),
                (bottom, "spacingBottom", None, false),
                (right, "spacingRight", None, false),
            ],
        );
        for (button, bit) in [(&self.bold, 1), (&self.italic, 2), (&self.underline, 4)] {
            let (me, set) = (Rc::downgrade(props), style(&send, "fontStyle"));
            button.connect_toggled(move |button| {
                let Some(me) = me.upgrade() else { return };
                let bits = match button.is_active() {
                    true => me.text.font_bits.get() | bit,
                    false => me.text.font_bits.get() & !bit,
                };
                me.text.font_bits.set(bits);
                set(Some(bits.to_string()));
            });
        }
        pick(&self.font, &send, "fontFamily", FONTS.to_vec());
        pick(&self.align, &send, "align", ALIGN.map(|(v, _)| v).to_vec());
        pick(
            &self.valign,
            &send,
            "verticalAlign",
            VALIGN.map(|(v, _)| v).to_vec(),
        );
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
                Some(Change::Style(owned(&pairs)))
            }),
        );
    }

    /// Show the first cell's `style`, a shape's own rows where `vertices` are selected.
    pub(super) fn fill(&self, style: &Resolved, vertices: bool) {
        // An arrow's label sits on its line, at no size to wrap to or turn across.
        self.label_position.set_visible(vertices);
        self.wrap.set_visible(vertices);
        self.vertical.set_visible(vertices);
        self.font_size.set_value(style.num("fontSize", 12.0));
        self.font_colour.fill(style.color("fontColor"));
        self.label_background
            .fill(style.color("labelBackgroundColor"));
        self.label_border.fill(style.color("labelBorderColor"));
        self.text_opacity.set_value(style.num("textOpacity", 100.0));
        let sides = [
            "spacing",
            "spacingTop",
            "spacingLeft",
            "spacingBottom",
            "spacingRight",
        ];
        for (row, key) in self.spacing.iter().zip(sides) {
            row.set_value(style.num(key, if key == "spacing" { 2.0 } else { 0.0 }));
        }
        self.vertical.set_active(!style.flag("horizontal", true));
        self.wrap
            .set_active(style.get("whiteSpace") == Some("wrap"));
        let bits = style.num("fontStyle", 0.0) as u32;
        self.font_bits.set(bits);
        self.bold.set_active(bits & 1 != 0);
        self.italic.set_active(bits & 2 != 0);
        self.underline.set_active(bits & 4 != 0);
        // A font draw.io's list does not have is an entry of its own, as draw.io's menu
        // names whatever font the cell has.
        let family = style.get("fontFamily").unwrap_or(FONTS[0]);
        let own: &[&str] = if FONTS.contains(&family) {
            &[]
        } else {
            &[family]
        };
        if let Some(fonts) = self.font.model().and_downcast::<gtk::StringList>() {
            let n = FONTS.len() as u32;
            fonts.splice(n, fonts.n_items() - n, own);
        }
        let font = FONTS.iter().position(|f| *f == family);
        self.font.set_selected(font.unwrap_or(FONTS.len()) as u32);
        self.align
            .set_selected(found(&ALIGN.map(|(v, _)| v), style.get("align")));
        // As draw.io reads it: a side named with no corner is its middle.
        let side = style.get("labelPosition").unwrap_or("center");
        let height = style.get("verticalLabelPosition").unwrap_or("middle");
        let placed = |height: &str| {
            POSITIONS
                .iter()
                .position(|(_, [s, h, ..])| *s == side && *h == height)
        };
        let position = placed(height).or_else(|| placed("middle")).unwrap_or(4);
        self.label_position.set_selected(position as u32);
        self.valign
            .set_selected(found(&VALIGN.map(|(v, _)| v), style.get("verticalAlign")));
    }
}
