//! The interactive form (ISO 32000-1 §12.7): each page's widget annotations and the fields they
//! show, with what a field inherits from its parents and from the form where it leaves it out,
//! read as pdf.js and pdfium read them.

use std::collections::HashSet;

use crate::{Dest, Dict, Object, Ref, Result, text_string};

use crate::Document;

/// How far up a field's parents are followed: no real form nests near it.
const DEPTH: usize = 32;
/// How many of the form's fields are looked at for scripts at most.
const FIELDS: usize = 10_000;

/// One widget annotation of a page and what the field it shows says.
#[derive(Debug, Clone, PartialEq)]
pub struct Widget {
    /// The annotation, and the terminal field whose value it shows: one object for a field merged
    /// with its only widget, the widget's `/Parent` for a field shown in several places.
    pub widget: Ref,
    pub field: Ref,
    /// The field's full name: its `/T` and its parents', outermost first, joined by `.`.
    pub name: String,
    /// `/FT`: `Tx`, `Btn`, `Ch` or `Sig`; empty for none.
    pub kind: Vec<u8>,
    /// `/Ff`.
    pub flags: u32,
    /// `/V` and `/DV`, a reference followed; null for none.
    pub value: Object,
    pub default: Object,
    /// `/TU`, the name a reader is shown.
    pub tip: Option<String>,
    pub max_len: Option<u32>,
    /// `/Q`: 0 left, 1 centred, 2 right.
    pub align: u8,
    /// `/DA`, the field's, a parent's or the form's.
    pub da: Vec<u8>,
    /// `/Opt`, each its export value and the text shown for it.
    pub options: Vec<(String, String)>,
    /// `/TI`, the option a list box shows first, and `/I`, the options picked, by place.
    pub top: usize,
    pub picked: Vec<usize>,
    /// `/Rect`, normalised, in the page's space.
    pub rect: [f32; 4],
    /// `/F` says Hidden or NoView.
    pub hidden: bool,
    /// The states `/AP /N` draws other than `Off`, a check box's or a radio button's "on", and
    /// `/AS`, the one drawn.
    pub states: Vec<Vec<u8>>,
    pub state: Option<Vec<u8>>,
    /// How the widget draws its box (`/MK`, `/BS`): turned by `/R`, a multiple of 90°, filled in
    /// `/BG` and outlined in `/BC`, each as RGB and `None` for none, by a border this wide.
    pub rotation: u16,
    pub background: Option<[f32; 3]>,
    pub border: Option<[f32; 3]>,
    pub border_width: f32,
    /// What clicking it does, a push button's.
    pub action: Option<Action>,
}

/// What clicking a push button does (ISO 32000-1 §12.6.4), of what accent carries out.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// ResetForm (§12.7.5.3): the fields it names, by reference or by full name, or with
    /// `exclude` every field but those; every field where it names none.
    Reset {
        fields: Vec<Object>,
        exclude: bool,
    },
    Uri(String),
    GoTo(Dest),
    /// A named action (§12.6.4.11): `NextPage`, `PrevPage`, `FirstPage` or `LastPage`.
    Named(Vec<u8>),
}

/// What the form as a whole says (`/AcroForm`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Form {
    /// `/NeedAppearances`: readers draw the fields from their values.
    pub need_appearances: bool,
    /// Whether it holds any field.
    pub fields: bool,
    /// An `/XFA` form beside the fields, and the stream of its `datasets` packet, which holds
    /// the fields' values again for an XFA reader.
    pub xfa: bool,
    pub datasets: Option<Ref>,
    /// A field computes, checks or formats its value by a script (`/AA` K, F, V or C), or the
    /// form has a calculation order (`/CO`).
    pub scripted: bool,
}

impl Document<'_> {
    /// The form, `None` for a document without one.
    pub fn form(&self) -> Result<Option<Form>> {
        let Some((need_appearances, datasets)) = self.form_fill()? else {
            return Ok(None);
        };
        let form = self.dict(&self.root()?, b"AcroForm")?;
        let calculated =
            matches!(self.resolve(form.get(b"CO"))?, Object::Array(co) if !co.is_empty());
        let fields = match self.resolve(form.get(b"Fields"))? {
            Object::Array(fields) => fields,
            _ => Vec::new(),
        };
        Ok(Some(Form {
            need_appearances,
            fields: !fields.is_empty(),
            xfa: form.get(b"XFA").is_some(),
            datasets,
            scripted: calculated || self.scripted(&fields)?,
        }))
    }

    /// What a fill needs to know of the form, without [`Document::form`]'s walk of its fields:
    /// whether it says `NeedAppearances`, and its XFA datasets' stream. `None` for no form.
    pub fn form_fill(&self) -> Result<Option<(bool, Option<Ref>)>> {
        let Object::Dict(form) = self.resolve(self.root()?.get(b"AcroForm"))? else {
            return Ok(None);
        };
        let need = matches!(
            self.resolve(form.get(b"NeedAppearances"))?,
            Object::Bool(true)
        );
        let mut datasets = None;
        if let Object::Array(packets) = self.resolve(form.get(b"XFA"))? {
            for [name, stream] in packets.as_chunks::<2>().0 {
                if matches!(self.resolve(Some(name))?, Object::String(n) if n == b"datasets") {
                    datasets = stream.reference();
                }
            }
        }
        Ok(Some((need, datasets)))
    }

    /// Whether a field under `fields`, or a widget of one, runs a script on a keystroke, to format
    /// or check its value, or to calculate it (`/AA` K, F, V, C): what accent does not run.
    fn scripted(&self, fields: &[Object]) -> Result<bool> {
        let (mut stack, mut seen) = (fields.to_vec(), HashSet::new());
        while let Some(node) = stack.pop() {
            if seen.len() >= FIELDS {
                break;
            }
            if node.reference().is_some_and(|r| !seen.insert(r)) {
                continue;
            }
            let Object::Dict(field) = self.resolve(Some(&node))? else {
                continue;
            };
            let aa = self.dict(&field, b"AA")?;
            if [&b"K"[..], b"F", b"V", b"C"]
                .iter()
                .any(|k| aa.get(k).is_some())
            {
                return Ok(true);
            }
            if let Object::Array(kids) = self.resolve(field.get(b"Kids"))? {
                stack.extend(kids);
            }
        }
        Ok(false)
    }

    /// What clicking the widget `annot` does, of what accent carries out: `None` for a script.
    fn action(&self, annot: &Dict) -> Result<Option<Action>> {
        let a = self.dict(annot, b"A")?;
        Ok(match a.get(b"S").and_then(Object::as_name) {
            Some(b"ResetForm") => {
                let fields = match self.resolve(a.get(b"Fields"))? {
                    Object::Array(fields) => fields,
                    _ => Vec::new(),
                };
                let flags = self.resolve(a.get(b"Flags"))?.as_int().unwrap_or(0);
                Some(Action::Reset {
                    fields,
                    exclude: flags & 1 != 0,
                })
            }
            Some(b"URI") => self.uri(annot)?.map(Action::Uri),
            Some(b"Named") => {
                (a.get(b"N").and_then(Object::as_name)).map(|n| Action::Named(n.to_vec()))
            }
            _ => self.goes(annot)?.map(Action::GoTo),
        })
    }

    /// The widgets of page `page`, in its `/Annots` order. One the array holds by value rather
    /// than by reference is left out, there being no object to write its value into.
    pub fn fields(&self, page: Ref) -> Result<Vec<Widget>> {
        let page = self.get(page)?;
        let annots = self.resolve(page.as_dict().and_then(|p| p.get(b"Annots")))?;
        let form = match self.resolve(self.root()?.get(b"AcroForm"))? {
            Object::Dict(form) => form,
            _ => Dict::default(),
        };
        let mut out = Vec::new();
        for entry in annots.as_array().unwrap_or_default() {
            let Some(at) = entry.reference() else {
                continue;
            };
            let Object::Dict(annot) = self.get(at)? else {
                continue;
            };
            if annot.get(b"Subtype").and_then(Object::as_name) != Some(b"Widget") {
                continue;
            }
            if let Some(widget) = self.widget(at, &annot, &form)? {
                out.push(widget);
            }
        }
        Ok(out)
    }

    /// The widget annotation `at` and its field, `None` for anything else.
    pub fn widget_at(&self, at: Ref) -> Result<Option<Widget>> {
        let form = match self.resolve(self.root()?.get(b"AcroForm"))? {
            Object::Dict(form) => form,
            _ => Dict::default(),
        };
        match self.get(at)? {
            Object::Dict(annot) => self.widget(at, &annot, &form),
            _ => Ok(None),
        }
    }

    /// A colour of `/MK` as RGB: none for an empty array, grey, RGB or CMYK by its length.
    fn colour(&self, mk: &Dict, key: &[u8]) -> Result<Option<[f32; 3]>> {
        let Object::Array(c) = self.resolve(mk.get(key))? else {
            return Ok(None);
        };
        let mut v = Vec::new();
        for n in &c {
            v.extend(crate::document::number(&self.resolve(Some(n))?));
        }
        Ok(match v[..] {
            [g] => Some([g; 3]),
            [r, g, b] => Some([r, g, b]),
            [c, m, y, k] => Some([c, m, y].map(|v| (1.0 - v) * (1.0 - k))),
            _ => None,
        })
    }

    /// The widget `at`, whose dictionary is `annot`, and its field; `None` for one of no field.
    fn widget(&self, at: Ref, annot: &Dict, form: &Dict) -> Result<Option<Widget>> {
        // The widget and its parents, nearest first, as far as a loop or `DEPTH` lets them go.
        let mut chain = vec![(at, annot.clone())];
        let mut seen = HashSet::from([at]);
        while chain.len() < DEPTH {
            let Some(parent) = chain[chain.len() - 1].1.get(b"Parent") else {
                break;
            };
            let Some(up) = parent.reference().filter(|r| seen.insert(*r)) else {
                break;
            };
            match self.get(up)? {
                Object::Dict(d) => chain.push((up, d)),
                _ => break,
            }
        }
        // A widget with no `/T` of its own shows its parent's field (§12.7.3.1).
        let field = match annot.get(b"T").is_none() && chain.len() > 1 {
            true => 1,
            false => 0,
        };
        let inherited = |key: &[u8]| -> Result<Object> {
            match chain.iter().find_map(|(_, d)| d.get(key)) {
                Some(found) => self.resolve(Some(found)),
                None => Ok(Object::Null),
            }
        };
        let Object::Name(kind) = inherited(b"FT")? else {
            return Ok(None);
        };
        let text = |o: Object| match o {
            Object::String(s) => Some(text_string(&s)),
            _ => None,
        };
        let mut names: Vec<String> = Vec::new();
        for (_, d) in &chain[field..] {
            names.extend(text(self.resolve(d.get(b"T"))?));
        }
        names.reverse();
        let own = &chain[field].1;
        let int = |o: Object| o.as_int();
        let da = match inherited(b"DA")? {
            Object::String(da) => da,
            _ => match self.resolve(form.get(b"DA"))? {
                Object::String(da) => da,
                _ => Vec::new(),
            },
        };
        let align = match int(inherited(b"Q")?) {
            Some(q) => q,
            None => int(self.resolve(form.get(b"Q"))?).unwrap_or(0),
        };
        let mut options = Vec::new();
        if let Object::Array(opt) = inherited(b"Opt")? {
            for option in &opt {
                options.extend(match self.resolve(Some(option))? {
                    Object::String(s) => Some((text_string(&s), text_string(&s))),
                    Object::Array(pair) if !pair.is_empty() => {
                        let export = text(self.resolve(pair.first())?).unwrap_or_default();
                        let shown = text(self.resolve(pair.get(1))?);
                        Some((export.clone(), shown.unwrap_or(export)))
                    }
                    _ => None,
                });
            }
        }
        let mut picked = Vec::new();
        if let Object::Array(i) = self.resolve(own.get(b"I"))? {
            for n in &i {
                picked.extend(int(self.resolve(Some(n))?).and_then(|n| usize::try_from(n).ok()));
            }
        }
        let normal = self.dict(&self.dict(annot, b"AP")?, b"N")?;
        let states = (normal.0.iter())
            .map(|(name, _)| name.clone())
            .filter(|name| name != b"Off")
            .collect();
        let flags = int(self.resolve(annot.get(b"F"))?).unwrap_or(0);
        let mk = self.dict(annot, b"MK")?;
        let rotation = int(self.resolve(mk.get(b"R"))?)
            .unwrap_or(0)
            .rem_euclid(360);
        let border_width = match self.dict(annot, b"BS")?.get(b"W") {
            Some(w) => crate::document::number(&self.resolve(Some(w))?),
            None => match self.resolve(annot.get(b"Border"))? {
                Object::Array(b) => crate::document::number(&self.resolve(b.get(2))?),
                _ => None,
            },
        };
        Ok(Some(Widget {
            widget: at,
            field: chain[field].0,
            name: names.join("."),
            kind,
            flags: int(inherited(b"Ff")?).map_or(0, |f| f as u32),
            value: inherited(b"V")?,
            default: inherited(b"DV")?,
            tip: text(self.resolve(own.get(b"TU"))?).filter(|t| !t.trim().is_empty()),
            max_len: int(inherited(b"MaxLen")?).and_then(|n| u32::try_from(n).ok()),
            align: u8::try_from(align).ok().filter(|q| *q <= 2).unwrap_or(0),
            da,
            options,
            top: (int(self.resolve(own.get(b"TI"))?))
                .and_then(|n| usize::try_from(n).ok())
                .unwrap_or(0),
            picked,
            rect: self.rect(annot.get(b"Rect"))?.unwrap_or_default(),
            hidden: flags & (2 | 32) != 0,
            states,
            state: self
                .resolve(annot.get(b"AS"))?
                .as_name()
                .map(<[u8]>::to_vec),
            rotation: u16::try_from(rotation - rotation % 90).unwrap_or(0),
            background: self.colour(&mk, b"BG")?,
            border: self.colour(&mk, b"BC")?,
            border_width: border_width.unwrap_or(1.0),
            action: self.action(annot)?,
        }))
    }
}
