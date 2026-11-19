//! Form fields (ISO 32000-1 §12.7) as a reader fills them in: each widget of a page with the
//! field it shows, read by `accent_pdf`, and what kind of field that is.

use accent_pdf::{Dict, Object, Ref, Widget, text_string};

use super::cos::{Objects, name};
use super::{PdfDoc, Rect};
use crate::{Error, Result};

/// `/Ff` bits (ISO 32000-1 Tables 221, 226, 228 and 230).
const READ_ONLY: u32 = 1;
const MULTILINE: u32 = 1 << 12;
const PASSWORD: u32 = 1 << 13;
const NO_TOGGLE_TO_OFF: u32 = 1 << 14;
const RADIO: u32 = 1 << 15;
const PUSH_BUTTON: u32 = 1 << 16;
const COMBO: u32 = 1 << 17;
const EDIT: u32 = 1 << 18;
const FILE_SELECT: u32 = 1 << 20;
const MULTI_SELECT: u32 = 1 << 21;
const COMB: u32 = 1 << 24;

/// One widget of a page and the field it shows: what a reader fills in there.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// The terminal field, which holds the value, and the widget annotation showing it here.
    pub field: Ref,
    pub widget: Ref,
    /// The widget's `/Rect`, in top-left page points.
    pub rect: Rect,
    /// The full name, and the one shown to a reader (`/TU`).
    pub name: String,
    pub tip: Option<String>,
    pub kind: FieldKind,
    pub read_only: bool,
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    /// A text field: its value, how it is laid out, and the font, size and colour `/DA` sets it
    /// in, a size of 0 being one fitted to the box.
    Text {
        value: String,
        multiline: bool,
        password: bool,
        comb: bool,
        max_len: Option<usize>,
        align: u8,
        da: Da,
    },
    /// A check box, `on` being the state its appearance draws when checked.
    Check {
        on: Vec<u8>,
        checked: bool,
    },
    /// One button of a radio group, `on` being the state it stands for.
    Radio {
        on: Vec<u8>,
        checked: bool,
        no_toggle_off: bool,
    },
    /// A combo box or a list box: its options, each an export value and the text shown, and the
    /// export values picked.
    Choice {
        combo: bool,
        edit: bool,
        multi: bool,
        options: Vec<(String, String)>,
        picked: Vec<String>,
    },
    /// A push button, which holds no value.
    Button,
    Signature,
}

/// A default appearance (`/DA`, ISO 32000-1 §12.7.3.3): the font the resources name, its size,
/// 0 for one fitted to the box, and the colour text is filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Da {
    pub font: Vec<u8>,
    pub size: f32,
    pub rgb: [u8; 3],
}

impl Da {
    /// `/Helv 14 Tf 1 0 0 rg`, a grey's `0 g` or a CMYK colour's `0 0 0 1 k`, in any order; the
    /// last of each operator counts, as a content stream's would. `None` without a `Tf`.
    pub fn parse(da: &[u8]) -> Option<Da> {
        let words: Vec<&str> = std::str::from_utf8(da).ok()?.split_whitespace().collect();
        let op = |op: &str| words.iter().rposition(|w| *w == op);
        let number = |at: Option<usize>| words.get(at?)?.parse::<f32>().ok();
        let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let tf = op("Tf")?;
        let size = number(tf.checked_sub(1))?;
        let font = (tf.checked_sub(2).and_then(|at| words.get(at)))
            .and_then(|w| w.strip_prefix('/'))
            .map_or_else(Vec::new, |w| w.as_bytes().to_vec());
        let back = |at: usize, n: usize| number(at.checked_sub(n)).unwrap_or(0.0);
        let colours = [("rg", 3), ("g", 1), ("k", 4)];
        let last = (colours.iter())
            .filter_map(|&(name, n)| Some((op(name)?, name, n)))
            .max_by_key(|(at, ..)| *at);
        let rgb = match last {
            Some((at, "rg", _)) => [3, 2, 1].map(|n| channel(back(at, n))),
            Some((at, "g", _)) => [channel(back(at, 1)); 3],
            Some((at, _, _)) => {
                let k = back(at, 1);
                [4, 3, 2].map(|n| channel((1.0 - back(at, n)) * (1.0 - k)))
            }
            None => [0; 3],
        };
        Some(Da { font, size, rgb })
    }
}

/// What a reader fills a field with.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    /// A check box or a radio button on, in the state named, or off.
    On(Option<Vec<u8>>),
}

/// What a fill wrote: each object it changed as it was before, which its Undo puts back, and each
/// page it changed with the box that covers what changed there.
#[derive(Debug, Clone, PartialEq)]
pub struct Filled {
    pub kept: Vec<(Ref, Object)>,
    pub areas: Vec<(usize, Rect)>,
}

impl PdfDoc {
    /// The widgets of `page` with their fields, in `/Annots` order.
    pub fn fields(&self, page: usize) -> Result<Vec<FormField>> {
        let at = self.cos.page(page)?;
        let widgets = self.reading(|doc| doc.fields(at))?;
        let (_, height) = self.page_size(page)?;
        Ok(widgets.into_iter().map(|w| field_of(w, height)).collect())
    }

    /// The terminal field `field` filled with `value`, as pdf.js saves a fill: a check box's or a
    /// radio group's `/V` the state turned on, or `/Off`, and `/AS` on each of its widgets that
    /// state where the widget draws it, `/Off` where it does not. The document is opened again
    /// once.
    pub fn fill(&mut self, field: Ref, value: &FieldValue) -> Result<Filled> {
        let widgets = self.cos.widgets_of(field)?;
        let kept = match value {
            FieldValue::On(on) => self.cos.put_state(field, &widgets, on.as_deref())?,
        };
        let areas = self.areas_of(&widgets)?;
        self.reopen()?;
        Ok(Filled { kept, areas })
    }

    /// The objects `kept` holds put back as they are there, each taking in its place what it
    /// replaces: a fill walked back by Undo, or forward again by Redo. The document is opened
    /// again once.
    pub fn swap(&mut self, kept: &mut [(Ref, Object)]) -> Result<()> {
        for (at, object) in kept.iter_mut() {
            let now = self.cos.get(*at)?;
            self.cos.update.set(*at, std::mem::replace(object, now));
        }
        self.reopen()
    }

    /// Each page `widgets` are on, with the box round those on it, in top-left page points.
    fn areas_of(&self, widgets: &[Ref]) -> Result<Vec<(usize, Rect)>> {
        let mut areas: Vec<(usize, Rect)> = Vec::new();
        for &widget in widgets {
            let Some(page) = self.cos.page_of(widget)? else {
                continue;
            };
            let dict = self.cos.dict(widget)?;
            let Some(file) = dict.get(b"Rect").and_then(|r| self.cos.area(r)) else {
                continue;
            };
            let (_, height) = self.page_size(page)?;
            let rect = Rect::from_corners(
                (file.left, height - file.bottom),
                (file.right, height - file.top),
            );
            match areas.iter_mut().find(|(p, _)| *p == page) {
                Some((_, area)) => *area = area.union(rect),
                None => areas.push((page, rect)),
            }
        }
        Ok(areas)
    }
}

impl Objects {
    /// The widgets of the terminal field `field`: its `/Kids`, or itself, a field merged with its
    /// only widget.
    pub(super) fn widgets_of(&self, field: Ref) -> Result<Vec<Ref>> {
        let dict = self.dict(field)?;
        let kids = match dict.get(b"Kids") {
            Some(kids) => self.resolve(kids)?,
            None => return Ok(vec![field]),
        };
        let kids = kids.as_array().unwrap_or_default().iter();
        Ok(kids.filter_map(Object::reference).collect())
    }

    /// The page the annotation `at` is on: the one its `/P` names when that page lists it, else
    /// the first whose `/Annots` does; `None` for one no page lists.
    pub(super) fn page_of(&self, at: Ref) -> Result<Option<usize>> {
        let listed =
            |page: usize| -> Result<bool> { Ok(self.annots(page)?.contains(&Object::Ref(at))) };
        let named = self.dict(at)?.get(b"P").and_then(Object::reference);
        if let Some(page) = named.and_then(|p| self.pages.iter().position(|&r| r == p))
            && listed(page)?
        {
            return Ok(Some(page));
        }
        for page in 0..self.pages.len() {
            if listed(page)? {
                return Ok(Some(page));
            }
        }
        Ok(None)
    }

    /// The check box or radio group `field`, shown by `widgets`, turned on in the state `on`, or
    /// off: its `/V`, and each widget's `/AS`. What each object was before.
    fn put_state(
        &mut self,
        field: Ref,
        widgets: &[Ref],
        on: Option<&[u8]>,
    ) -> Result<Vec<(Ref, Object)>> {
        let mut edits = Edits::default();
        let value = name(on.unwrap_or(b"Off"));
        edits.edit(self, field, |d| d.set(b"V", value.clone()))?;
        for &widget in widgets {
            let dict = self.dict(widget)?;
            let normal = match dict.get(b"AP") {
                Some(ap) => self.resolve(ap)?,
                None => Object::Null,
            };
            let normal = match normal.as_dict().and_then(|ap| ap.get(b"N")) {
                Some(n) => self.resolve(n)?,
                None => Object::Null,
            };
            let draws = |state: &[u8]| normal.as_dict().is_some_and(|n| n.get(state).is_some());
            let state = match on {
                Some(on) if draws(on) => name(on),
                _ => name(b"Off"),
            };
            edits.edit(self, widget, |d| d.set(b"AS", state.clone()))?;
        }
        Ok(edits.write(self))
    }
}

/// The dictionaries a fill changes, each read once and kept as it was, written together at the
/// end: a field merged with its widget is one object edited twice.
#[derive(Default)]
struct Edits(Vec<(Ref, Object, Dict)>);

impl Edits {
    /// `change` made to the dictionary `at`, as the earlier edits left it.
    fn edit(&mut self, cos: &Objects, at: Ref, change: impl FnOnce(&mut Dict)) -> Result<()> {
        let at_now = match self.0.iter_mut().find(|(r, ..)| *r == at) {
            Some((_, _, dict)) => dict,
            None => {
                let was = cos.get(at)?;
                let dict = (was.as_dict().cloned())
                    .ok_or_else(|| Error::Pdf(format!("object {} is no dictionary", at.num)))?;
                self.0.push((at, was, dict));
                &mut self.0.last_mut().expect("pushed above").2
            }
        };
        change(at_now);
        Ok(())
    }

    /// Every dictionary edited written into the session's update: what each was before.
    fn write(self, cos: &mut Objects) -> Vec<(Ref, Object)> {
        (self.0.into_iter())
            .map(|(at, was, now)| {
                cos.update.set(at, Object::Dict(now));
                (at, was)
            })
            .collect()
    }
}

/// What `w` is to a reader, on a page `height` points tall.
fn field_of(w: Widget, height: f32) -> FormField {
    let [left, bottom, right, top] = w.rect;
    let flag = |bit: u32| w.flags & bit != 0;
    let string = |o: &Object| match o {
        Object::String(s) => Some(text_string(s)),
        _ => None,
    };
    let on = w.states.first().cloned().unwrap_or_else(|| b"Yes".to_vec());
    // The state drawn, else the field's value where the widget names none.
    let checked = match &w.state {
        Some(state) => *state == on,
        None => w.value.as_name() == Some(&on[..]),
    };
    let kind = match &w.kind[..] {
        b"Tx" => {
            let (multiline, password) = (flag(MULTILINE), flag(PASSWORD));
            let max_len = w.max_len.filter(|n| *n > 0).map(|n| n as usize);
            FieldKind::Text {
                value: string(&w.value).unwrap_or_default(),
                multiline,
                password,
                comb: flag(COMB)
                    && !multiline
                    && !password
                    && !flag(FILE_SELECT)
                    && max_len.is_some(),
                max_len,
                align: w.align,
                da: Da::parse(&w.da).unwrap_or(Da {
                    font: b"Helv".to_vec(),
                    size: 0.0,
                    rgb: [0; 3],
                }),
            }
        }
        b"Btn" if flag(PUSH_BUTTON) => FieldKind::Button,
        b"Btn" if flag(RADIO) => FieldKind::Radio {
            on,
            checked,
            no_toggle_off: flag(NO_TOGGLE_TO_OFF),
        },
        b"Btn" => FieldKind::Check { on, checked },
        b"Ch" => {
            let picked = match &w.value {
                Object::Array(values) => values.iter().filter_map(string).collect(),
                other => match string(other) {
                    Some(value) => vec![value],
                    None => (w.picked.iter())
                        .filter_map(|&i| Some(w.options.get(i)?.0.clone()))
                        .collect(),
                },
            };
            FieldKind::Choice {
                combo: flag(COMBO),
                edit: flag(COMBO) && flag(EDIT),
                multi: flag(MULTI_SELECT),
                options: w.options,
                picked,
            }
        }
        // A signature, or a kind no reader fills in.
        _ => FieldKind::Signature,
    };
    FormField {
        field: w.field,
        widget: w.widget,
        rect: Rect {
            left,
            top: height - top,
            right,
            bottom: height - bottom,
        },
        name: w.name,
        tip: w.tip,
        kind,
        read_only: flag(READ_ONLY),
        hidden: w.hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_appearance_reads_its_font_size_and_colour() {
        let da = |s: &str| Da::parse(s.as_bytes());
        let red = da("/Helv 14 Tf 1 0 0 rg").unwrap();
        assert_eq!(
            (red.font, red.size, red.rgb),
            (b"Helv".to_vec(), 14.0, [255, 0, 0])
        );
        let grey = da("0.5 g /Time 0 Tf").unwrap();
        assert_eq!(
            (grey.font, grey.size, grey.rgb),
            (b"Time".to_vec(), 0.0, [128; 3])
        );
        assert_eq!(da("/Cour 9 Tf 0 0 0 1 k").unwrap().rgb, [0; 3]);
        assert_eq!(da("/F1 9 Tf 1 0 0 rg 0 g").unwrap().rgb, [0; 3]);
        assert_eq!(da("1 0 0 rg"), None);
    }
}
