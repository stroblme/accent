//! Form fields (ISO 32000-1 §12.7) as a reader fills them in: each widget of a page with the
//! field it shows, read by `accent_pdf`, and what kind of field that is.

use accent_pdf::{Dict, Object, Ref, SimpleFont, Widget, form, text_string};

use super::appearance::{self, Setting};
use super::cos::{Objects, dict, helvetica as helvetica_dict, name, reals, text};
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

/// What accent's own Helvetica is called in an appearance's resources.
const HELV: &[u8] = b"Helv";

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

impl FormField {
    /// The size a text field's editor shows its text at, in points, as pdf.js's does: its
    /// `/DA`'s, 9 for one fitted to the box, and never taller than the box, less a 1 pt border
    /// round it, has room for a line of, or on several lines for the lines that size leaves it
    /// (`_setTextStyle`).
    pub fn editor_size(&self) -> f32 {
        let (size, multiline) = match &self.kind {
            FieldKind::Text { da, multiline, .. } if da.size > 0.0 => (da.size, *multiline),
            FieldKind::Text { multiline, .. } => (9.0, *multiline),
            _ => return 9.0,
        };
        let one_place = |v: f32| (v * 10.0).round() / 10.0;
        let height = (self.rect.height() - 2.0).abs();
        let line = match multiline {
            true => height / ((height / (1.35 * size)).round().max(1.0)),
            false => height,
        };
        size.min(one_place(line / 1.35))
    }
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
    /// What a text field says.
    Text(String),
    /// The options a combo box or a list box picks, by export value.
    Choice(Vec<String>),
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
            FieldValue::Text(text) => {
                let (read, need) = self.widgets_read(&widgets)?;
                self.cos.put_text_value(field, &read, text, need)?
            }
            FieldValue::Choice(picked) => {
                let (read, need) = self.widgets_read(&widgets)?;
                self.cos.put_choice(field, &read, picked, need)?
            }
        };
        let areas = self.areas_of(&widgets)?;
        self.reopen()?;
        Ok(Filled { kept, areas })
    }

    /// `widgets` as `accent_pdf` reads them, and whether the form says `NeedAppearances`.
    fn widgets_read(&self, widgets: &[Ref]) -> Result<(Vec<Widget>, bool)> {
        let mut read = Vec::new();
        for &at in widgets {
            read.extend(self.reading(|doc| doc.widget_at(at))?);
        }
        let form = self.reading(|doc| doc.form())?;
        Ok((read, form.is_some_and(|form| form.need_appearances)))
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

impl Objects {
    /// The text field `field`, shown by `widgets`, made to say `value`, as pdf.js saves one: its
    /// `/V`, without the rich text an editor may have added, and each widget an appearance of
    /// ours. What each object was before.
    fn put_text_value(
        &mut self,
        field: Ref,
        widgets: &[Widget],
        value: &str,
        need: bool,
    ) -> Result<Vec<(Ref, Object)>> {
        let mut edits = Edits::default();
        edits.edit(self, field, |d| {
            d.set(b"V", text(value));
            d.remove(b"RV");
        })?;
        let made = (widgets.iter())
            .map(|w| match need {
                true => Ok(None),
                false => self.text_appearance(w, value),
            })
            .collect::<Result<Vec<_>>>()?;
        self.put_appearances(&mut edits, widgets, need, made)?;
        Ok(edits.write(self))
    }

    /// The combo box or list box `field`, shown by `widgets`, made to pick the export values
    /// `picked`, as pdf.js saves one: its `/V`, one string, or an array for a list box that
    /// picks several or none; its `/I`, the options picked by place, where it had one or picks
    /// several; and each widget an appearance of ours, a combo box's the option's text as a text
    /// field's, a list box's its options. What each object was before.
    fn put_choice(
        &mut self,
        field: Ref,
        widgets: &[Widget],
        picked: &[String],
        need: bool,
    ) -> Result<Vec<(Ref, Object)>> {
        let Some(first) = widgets.first() else {
            return Ok(Vec::new());
        };
        let multi = first.flags & MULTI_SELECT != 0;
        let combo = first.flags & COMBO != 0;
        let value = match (picked, multi) {
            ([one], _) => text(one),
            (_, true) => Object::Array(picked.iter().map(|p| text(p)).collect()),
            ([], false) => text(""),
            (many, false) => text(&many[0]),
        };
        let indices: Vec<Object> = (first.options.iter().enumerate())
            .filter(|(_, (export, _))| picked.contains(export))
            .map(|(i, _)| Object::Int(i as i64))
            .collect();
        let had = self.dict(field)?.get(b"I").is_some();
        let mut edits = Edits::default();
        edits.edit(self, field, |d| {
            d.set(b"V", value);
            if had || multi {
                d.set(b"I", Object::Array(indices));
            }
        })?;
        let made = (widgets.iter())
            .map(|w| match (need, combo) {
                (true, _) => Ok(None),
                (false, true) => {
                    let shown = picked.first().map(|p| {
                        let option = w.options.iter().find(|(export, _)| export == p);
                        option.map_or(p.as_str(), |(_, shown)| shown.as_str())
                    });
                    self.appearance(w, |set| {
                        appearance::text(w, shown.unwrap_or_default(), set, false, None)
                    })
                }
                (false, false) => self.appearance(w, |set| appearance::list(w, picked, set)),
            })
            .collect::<Result<Vec<_>>>()?;
        self.put_appearances(&mut edits, widgets, need, made)?;
        Ok(edits.write(self))
    }

    /// Each of `widgets` given its appearance in `made`. In a form that says `NeedAppearances`,
    /// whose readers draw the fields from their values, the widgets' appearances go instead; and
    /// where `made` holds none, no font having a code for a character of the value, the form is
    /// made to say so (user decision 2).
    fn put_appearances(
        &mut self,
        edits: &mut Edits,
        widgets: &[Widget],
        need: bool,
        made: Vec<Option<accent_pdf::Stream>>,
    ) -> Result<()> {
        let mut unset = false;
        for (w, stream) in widgets.iter().zip(made) {
            match stream {
                Some(stream) => {
                    let at = self.update.add(Object::Stream(stream));
                    let ap = Object::Dict(dict([(b"N", Object::Ref(at))]));
                    edits.edit(self, w.widget, |d| d.set(b"AP", ap))?;
                }
                None => {
                    edits.edit(self, w.widget, |d| d.remove(b"AP"))?;
                    unset |= !need;
                }
            }
        }
        if unset {
            self.need_appearances(edits)?;
        }
        Ok(())
    }

    /// `w`'s appearance saying `value`: one line or several, or a comb's cells.
    fn text_appearance(&self, w: &Widget, value: &str) -> Result<Option<accent_pdf::Stream>> {
        let flag = |bit: u32| w.flags & bit != 0;
        let multiline = flag(MULTILINE);
        let comb = (w.max_len.filter(|n| *n > 0))
            .filter(|_| flag(COMB) && !multiline && !flag(PASSWORD) && !flag(FILE_SELECT))
            .map(|n| n as usize);
        self.appearance(w, |set| appearance::text(w, value, set, multiline, comb))
    }

    /// `w`'s appearance as `draw` sets it: in its `/DA`'s font where that has a code for each
    /// character it sets, else in Helvetica, which every reader has; `None` where neither does.
    fn appearance(
        &self,
        w: &Widget,
        draw: impl Fn(&Setting) -> Option<Vec<u8>>,
    ) -> Result<Option<accent_pdf::Stream>> {
        let da = Da::parse(&w.da).unwrap_or(Da {
            font: HELV.to_vec(),
            size: 0.0,
            rgb: [0; 3],
        });
        let found = self.field_font(w.widget, &da.font)?;
        let own = (found.as_ref()).and_then(|(_, font)| {
            SimpleFont::of(font, |o| match o {
                Some(o) => self.resolve(o).unwrap_or(Object::Null),
                None => Object::Null,
            })
        });
        let helvetica = SimpleFont::helvetica();
        let choices = [
            own.as_ref()
                .zip(found.as_ref())
                .map(|(font, (entry, _))| (&da.font[..], font, entry.clone())),
            Some((HELV, &helvetica, Object::Dict(helvetica_dict()))),
        ];
        for (name, font, entry) in choices.into_iter().flatten() {
            let set = Setting {
                name,
                font,
                size: da.size,
                rgb: da.rgb,
            };
            let Some(content) = draw(&set) else {
                continue;
            };
            let fonts = Object::Dict(Dict(vec![(name.to_vec(), entry)]));
            let mut stream = form(appearance::bbox(w), content, dict([(b"Font", fonts)]));
            if let Some(m) = appearance::turned(w) {
                stream.dict.set(b"Matrix", reals(m));
            }
            return Ok(Some(stream));
        }
        Ok(None)
    }

    /// The font the resources call `name` where a field's appearance finds it: in the widget's
    /// own `/DR`, the resources of the appearance it has, or the form's `/DR`, as pdf.js looks
    /// for it. What names it there, and its dictionary.
    fn field_font(&self, widget: Ref, name: &[u8]) -> Result<Option<(Object, Dict)>> {
        let annot = self.dict(widget)?;
        let follow = |d: &Dict, key: &[u8]| -> Result<Object> {
            match d.get(key) {
                Some(o) => self.resolve(o),
                None => Ok(Object::Null),
            }
        };
        let mut places = vec![follow(&annot, b"DR")?];
        if let Some(ap) = follow(&annot, b"AP")?.as_dict() {
            let normal = follow(ap, b"N")?;
            if let Some(n) = normal.as_dict() {
                places.push(follow(n, b"Resources")?);
            }
        }
        if let Some(form) = self.acro_form()?.1 {
            places.push(follow(&form, b"DR")?);
        }
        for resources in places {
            let Some(resources) = resources.as_dict() else {
                continue;
            };
            let fonts = follow(resources, b"Font")?;
            let Some(entry) = fonts.as_dict().and_then(|f| f.get(name)) else {
                continue;
            };
            if let Some(font) = self.resolve(entry)?.as_dict() {
                return Ok(Some((entry.clone(), font.clone())));
            }
        }
        Ok(None)
    }

    /// The form dictionary: the object it is, `None` where the catalog holds it, and what it
    /// says; nothing for a document without one.
    fn acro_form(&self) -> Result<(Option<Ref>, Option<Dict>)> {
        let Some(root) = self.pdf.trailer().get(b"Root").and_then(Object::reference) else {
            return Ok((None, None));
        };
        match self.dict(root)?.get(b"AcroForm") {
            Some(&Object::Ref(at)) => Ok((Some(at), self.dict(at).ok())),
            Some(Object::Dict(d)) => Ok((None, Some(d.clone()))),
            _ => Ok((None, None)),
        }
    }

    /// The form made to say `NeedAppearances`, in its own object or in the catalog holding it.
    fn need_appearances(&self, edits: &mut Edits) -> Result<()> {
        let set = |d: &mut Dict| d.set(b"NeedAppearances", Object::Bool(true));
        match self.acro_form()? {
            (Some(at), _) => edits.edit(self, at, set),
            (None, Some(_)) => {
                let root = self.pdf.trailer().get(b"Root").and_then(Object::reference);
                let root = root.ok_or_else(|| Error::Pdf("no catalog".to_string()))?;
                edits.edit(self, root, |catalog| {
                    if let Some(Object::Dict(form)) = catalog
                        .0
                        .iter_mut()
                        .find_map(|(k, v)| (k == b"AcroForm").then_some(v))
                    {
                        set(form);
                    }
                })
            }
            (None, None) => Ok(()),
        }
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
