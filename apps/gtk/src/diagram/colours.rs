//! The colour swatches on a diagram's ring: the colours the diagram's cells were given most
//! recently, and the colour a pick gives new cells and the selection.

use std::collections::HashMap;
use std::rc::Rc;

use accent_drawio::{CellId, Color, File, Style};

use super::DiagramTab;

/// How many swatches the ring shows, as many as the PDF ring's.
pub const SLOTS: usize = 6;

/// The keys a cell's colours are written in, which the swatches are taken from.
const KEYS: [&str; 3] = ["fillColor", "strokeColor", "fontColor"];

/// The key a swatch's colour goes into on a cell: a line's stroke, a text's font and any other
/// shape's fill. A group and a picture take none, as filling one would cover what it holds.
pub fn key_of(style: &Style, edge: bool) -> Option<&'static str> {
    if edge {
        return Some("strokeColor");
    }
    if style.names("group") || style.resolve(false).shape() == "image" {
        return None;
    }
    match style.names("text") || style.names("edgeLabel") {
        true => Some("fontColor"),
        false => Some("fillColor"),
    }
}

/// `style` in `colour`, written where a cell of that style shows one ([`key_of`]).
pub fn painted(style: &str, edge: bool, colour: [u8; 3]) -> String {
    let mut style = Style::parse(style);
    if let Some(key) = key_of(&style, edge) {
        style.set(key, Some(&hex(colour)));
    }
    style.to_string()
}

/// A colour as a style writes it.
pub fn hex([r, g, b]: [u8; 3]) -> String {
    Color::rgb(r, g, b).hex()
}

/// Where a cell's colour is: its page, its id and the key.
type Place = (usize, CellId, &'static str);

/// What the swatches show for one diagram, and the colour picked on them.
#[derive(Default)]
pub struct Swatches {
    /// The colour new cells are drawn in; `None` leaves them their style's own.
    pub picked: Option<[u8; 3]>,
    slots: Vec<[u8; 3]>,
    /// Every cell's colours at the last look, to tell which a change set; `None` before it.
    seen: Option<HashMap<Place, [u8; 3]>>,
}

impl Swatches {
    /// The swatches for `file` as it now stands. The first look takes the file's colours, the
    /// last cell's first since draw.io adds a cell after the rest, and `fill` after them. Then a
    /// colour a change set that is not on the ring comes in first, pushing the last one off,
    /// while one on the ring keeps its place, so a swatch never moves under the pointer that
    /// picked it.
    pub fn update(&mut self, file: &File, fill: &[[u8; 3]]) -> &[[u8; 3]] {
        let now = colours(file);
        match &self.seen {
            None => {
                self.slots.clear();
                let all = now
                    .iter()
                    .rev()
                    .map(|(_, c)| *c)
                    .chain(fill.iter().copied());
                for c in all {
                    if self.slots.len() < SLOTS && !self.slots.contains(&c) {
                        self.slots.push(c);
                    }
                }
            }
            Some(seen) => {
                for (place, c) in &now {
                    if seen.get(place) != Some(c) && !self.slots.contains(c) {
                        self.slots.insert(0, *c);
                        self.slots.truncate(SLOTS);
                    }
                }
            }
        }
        self.seen = Some(now.into_iter().collect());
        &self.slots
    }
}

/// Every colour the cells of `file` are given, in document order.
fn colours(file: &File) -> Vec<(Place, [u8; 3])> {
    let mut all = Vec::new();
    for (i, page) in file.pages().iter().enumerate() {
        for cell in &page.cells {
            for key in KEYS {
                let colour = cell.style.get(key).and_then(Color::parse);
                if let Some(c) = colour.filter(|c| c.a > 0) {
                    all.push(((i, cell.id.clone(), key), [c.r, c.g, c.b]));
                }
            }
        }
    }
    all
}

impl DiagramTab {
    /// Show the swatches on the ring as the diagram now stands.
    pub(super) fn show_swatches(&self) {
        let fill = crate::theme::swatches().map(|s| s.unwrap_or_else(crate::theme::accent_rgb));
        let (slots, picked) = {
            let mut swatches = self.swatches.borrow_mut();
            let slots = swatches.update(self.editor.borrow().file(), &fill).to_vec();
            (slots, swatches.picked)
        };
        self.ring.set_swatches(slots, picked);
    }

    /// A style in the colour picked on the ring, if there is one.
    pub(super) fn coloured(&self, style: String, edge: bool) -> String {
        match self.swatches.borrow().picked {
            Some(colour) => painted(&style, edge, colour),
            None => style,
        }
    }

    /// A swatch picked on the ring: the colour new cells are drawn in, and the selection's as
    /// one undo step. The swatch already picked, with nothing selected, puts the colour down.
    pub(super) fn ring_colour(self: &Rc<Self>, colour: [u8; 3]) {
        let ids = self.selection();
        {
            let mut swatches = self.swatches.borrow_mut();
            let down = ids.is_empty() && swatches.picked == Some(colour);
            swatches.picked = (!down).then_some(colour);
        }
        let keyed: Vec<(&str, CellId)> = {
            let editor = self.editor.borrow();
            let page = editor.page(self.page_index.get()).ok();
            let key = |id: &CellId| {
                let cell = page?.cell(id)?;
                Some((key_of(&cell.style, cell.edge)?, id.clone()))
            };
            ids.iter().filter_map(key).collect()
        };
        if !keyed.is_empty() {
            let value = hex(colour);
            self.edit(|e, page| {
                e.at_once(|e| {
                    for key in KEYS {
                        let ids: Vec<CellId> = (keyed.iter())
                            .filter(|(k, _)| *k == key)
                            .map(|(_, id)| id.clone())
                            .collect();
                        if !ids.is_empty() {
                            e.set_style(page, &ids, key, Some(&value))?;
                        }
                    }
                    Ok(())
                })
            });
        }
        self.show_swatches();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_drawio::{Editor, Rect};

    const FILL: [[u8; 3]; 6] = [
        [1, 0, 0],
        [2, 0, 0],
        [3, 0, 0],
        [4, 0, 0],
        [5, 0, 0],
        [0, 0, 0],
    ];

    /// The file's colours come first, the last cell's first and each once, the theme's after
    /// them; a colour set afterwards comes in first only when it is not on the ring already.
    #[test]
    fn the_swatches_hold_the_newest_colours_and_keep_their_places() {
        let mut editor = Editor::new(File::blank());
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        let add = |e: &mut Editor, style: &str| e.add_vertex(0, None, rect, style, "").unwrap();
        add(&mut editor, "fillColor=#ff0000;strokeColor=#000000;");
        let b = add(&mut editor, "fillColor=#00ff00;fontColor=none;");
        add(&mut editor, "text;fontColor=#0000FF;");
        let mut swatches = Swatches::default();
        let (red, green, blue) = ([255, 0, 0], [0, 255, 0], [0, 0, 255]);
        assert_eq!(
            swatches.update(editor.file(), &FILL),
            [blue, green, [0, 0, 0], red, FILL[0], FILL[1]]
        );
        // Red is on the ring: setting it keeps every swatch where it was.
        editor
            .set_style(0, std::slice::from_ref(&b), "fillColor", Some("#ff0000"))
            .unwrap();
        let before = swatches.slots.clone();
        assert_eq!(swatches.update(editor.file(), &FILL), before);
        // A colour off the ring comes in first and pushes the last one off.
        editor
            .set_style(0, &[b], "strokeColor", Some("#123456"))
            .unwrap();
        assert_eq!(
            swatches.update(editor.file(), &FILL),
            [[0x12, 0x34, 0x56], blue, green, [0, 0, 0], red, FILL[0]]
        );
    }

    #[test]
    fn a_swatch_colours_a_line_s_stroke_a_text_s_font_and_a_shape_s_fill() {
        let colour = [0xab, 0xcd, 0xef];
        assert_eq!(
            painted("endArrow=none;", true, colour),
            "endArrow=none;strokeColor=#abcdef;"
        );
        assert_eq!(
            painted("text;html=1;", false, colour),
            "text;html=1;fontColor=#abcdef;"
        );
        assert_eq!(
            painted("shape=note;", false, colour),
            "shape=note;fillColor=#abcdef;"
        );
        assert_eq!(painted("group;", false, colour), "group;");
        assert_eq!(painted("shape=image;", false, colour), "shape=image;");
    }
}
