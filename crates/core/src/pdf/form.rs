//! Form fields (ISO 32000-1 §12.7) as a reader fills them in: each widget of a page with the
//! field it shows, read by `accent_pdf`, and what kind of field that is.

use accent_pdf::{Object, Ref, Widget, text_string};

use super::{PdfDoc, Rect};
use crate::Result;

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

impl PdfDoc {
    /// The widgets of `page` with their fields, in `/Annots` order.
    pub fn fields(&self, page: usize) -> Result<Vec<FormField>> {
        let at = self.cos.page(page)?;
        let widgets = self.reading(|doc| doc.fields(at))?;
        let (_, height) = self.page_size(page)?;
        Ok(widgets.into_iter().map(|w| field_of(w, height)).collect())
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
