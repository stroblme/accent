//! Form fields: what accent reads of them against what pdfium reads, and what pdfium draws of a
//! widget, from its appearance stream or from its value alone.

use accent_pdf::Object;
use pdfium_render::prelude::*;

use super::super::*;
use super::{open_pdf, pdf_of};

/// The fixture form, `edit` rewriting its objects' text first.
pub(super) fn form_pdf(edit: impl Fn(String) -> String) -> Vec<u8> {
    let objs: Vec<String> = accent_pdf::fixtures::form_objects()
        .into_iter()
        .map(edit)
        .collect();
    pdf_of(&objs)
}

/// Whether page `page`, rendered at 2 px a point, has a pixel `like` says inside `r`.
fn painted(doc: &PdfDoc, page: usize, r: Rect, like: impl Fn([u8; 4]) -> bool) -> bool {
    let image = doc.render_page(page, 2.0, Theme::Plain).unwrap();
    let w = image.width as usize;
    (image.data.as_chunks::<4>().0.iter().enumerate()).any(|(i, &px)| {
        let (x, y) = ((i % w) as f32 / 2.0, (i / w) as f32 / 2.0);
        r.contains((x, y)) && like(px)
    })
}

fn red(px: [u8; 4]) -> bool {
    px[0] > 200 && px[1] < 100 && px[2] < 100
}

fn green(px: [u8; 4]) -> bool {
    px[1] > 100 && px[0] < 80 && px[2] < 80
}

/// The field named `name` on `page`.
fn field<'f>(fields: &'f [FormField], name: &str) -> &'f FormField {
    (fields.iter().find(|f| f.name == name)).unwrap_or_else(|| panic!("no field {name}"))
}

/// pdfium draws a widget from its appearance stream, the state `/AS` names, through the form
/// it opens every document with (`FPDF_FFLDraw`): a check box's red cross once `/AS` is its
/// on-state and nothing while it is `/Off`, and the radio button whose `/AS` is on alone.
#[test]
fn a_widget_draws_its_appearance() {
    let Some((_dir, off)) = open_pdf(&form_pdf(|o| o)) else {
        return;
    };
    let checked = |o: String| o.replace("/V/Off/AS/Off", "/V/Yes/AS/Yes");
    let (_dir2, on) = open_pdf(&form_pdf(checked)).unwrap();
    let fields = off.fields(0).unwrap();
    let agree = field(&fields, "agree").rect;
    assert!(!painted(&off, 0, agree, red), "unchecked");
    assert!(painted(&on, 0, agree, red), "checked");
    let buttons: Vec<Rect> = (fields.iter())
        .filter(|f| f.name == "size")
        .map(|f| f.rect)
        .collect();
    assert_eq!(buttons.len(), 2);
    assert!(!painted(&off, 0, buttons[0], green));
    assert!(painted(&off, 0, buttons[1], green));
}

/// A text field with no appearance stream of its own in a form that says `NeedAppearances` is
/// drawn by pdfium from its value, in its `/DA`'s colour. A character the `/DA` font cannot
/// encode is in the file and reads back, as pdfium reads it too, but pdfium draws the field
/// without it, as poppler does: "Hi α" is drawn as "Hi".
#[test]
fn need_appearances_draws_the_value_without_an_ap() {
    let page = |value: &str| {
        pdf_of(&[
            "<</Type/Catalog/Pages 2 0 R/AcroForm<</Fields[4 0 R]/NeedAppearances true\
             /DR<</Font<</Helv 5 0 R>>>>>>>>"
                .to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]/Annots[4 0 R]>>".to_string(),
            format!(
                "<</Type/Annot/Subtype/Widget/FT/Tx/T(t)/F 4/P 3 0 R/Rect[10 40 190 70]\
                 /DA(/Helv 18 Tf 1 0 0 rg)/V{value}>>"
            ),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>".to_string(),
        ])
    };
    let Some((_dir, latin)) = open_pdf(&page("(Hello)")) else {
        return;
    };
    let box_ = Rect::from_corners((10.0, 30.0), (190.0, 60.0));
    assert!(painted(&latin, 0, box_, red), "Latin");
    let (_dir, empty) = open_pdf(&page("()")).unwrap();
    assert!(!painted(&empty, 0, box_, red), "empty");

    let (_dir, hi) = open_pdf(&page("(Hi)")).unwrap();
    let (_dir, alpha) = open_pdf(&page("<FEFF00480069002003B1>")).unwrap();
    let FieldKind::Text { value, .. } = &alpha.fields(0).unwrap()[0].kind else {
        panic!()
    };
    assert_eq!(value, "Hi α");
    {
        let _guard = lock();
        let page = alpha.doc().pages().get(0).unwrap();
        let read =
            (page.annotations().iter()).find_map(|a| a.as_form_field()?.as_text_field()?.value());
        assert_eq!(read.as_deref(), Some("Hi α"), "as pdfium reads it");
    }
    // Its red pixels and how far across they reach, which pdfium's line set a pixel lower for
    // the glyph it could not find leaves as they were.
    let drawn = |doc: &PdfDoc| {
        let image = doc.render_page(0, 2.0, Theme::Plain).unwrap();
        let w = image.width as usize;
        let reds = (image.data.as_chunks::<4>().0.iter().enumerate()).filter(|(_, px)| red(**px));
        let xs: Vec<usize> = reds.map(|(i, _)| i % w).collect();
        (xs.len(), xs.iter().min().copied(), xs.iter().max().copied())
    };
    assert!(painted(&alpha, 0, box_, red));
    assert_eq!(drawn(&alpha), drawn(&hi), "the α is not drawn");
}

/// Every field accent reads is the one pdfium reads, by name, kind, value and box.
#[test]
fn fields_read_as_pdfium_reads_them() {
    let Some((_dir, doc)) = open_pdf(&form_pdf(|o| o)) else {
        return;
    };
    let mut ours: Vec<(usize, String, String)> = Vec::new();
    for page in 0..doc.page_count() {
        for f in doc.fields(page).unwrap() {
            let value = match &f.kind {
                FieldKind::Text { value, .. } => value.clone(),
                FieldKind::Check { checked, .. } => checked.to_string(),
                FieldKind::Radio { checked, .. } => checked.to_string(),
                FieldKind::Choice { picked, .. } => picked.join(","),
                FieldKind::Button | FieldKind::Signature => String::new(),
            };
            ours.push((page, f.name, value));
        }
    }
    let guard = lock();
    let mut theirs = Vec::new();
    for (page, p) in doc.doc().pages().iter().enumerate() {
        for annot in p.annotations().iter() {
            let Some(field) = annot.as_form_field() else {
                continue;
            };
            let value = match field.field_type() {
                PdfFormFieldType::Text => field.as_text_field().unwrap().value(),
                PdfFormFieldType::Checkbox => Some(
                    field
                        .as_checkbox_field()
                        .unwrap()
                        .is_checked()
                        .unwrap()
                        .to_string(),
                ),
                PdfFormFieldType::RadioButton => Some(
                    field
                        .as_radio_button_field()
                        .unwrap()
                        .is_checked()
                        .unwrap()
                        .to_string(),
                ),
                PdfFormFieldType::ComboBox => field.as_combo_box_field().unwrap().value(),
                PdfFormFieldType::ListBox => field.as_list_box_field().unwrap().value(),
                _ => None,
            };
            theirs.push((
                page,
                field.name().unwrap_or_default(),
                value.unwrap_or_default(),
            ));
        }
    }
    drop(guard);
    // pdfium's list box value is its first pick, and its combo box's what it shows.
    for (_, name, value) in &mut ours {
        match name.as_str() {
            "fruit" => *value = "Apple".to_string(),
            "country" => *value = "France".to_string(),
            _ => {}
        }
    }
    assert_eq!(ours, theirs);
    let fields = doc.fields(0).unwrap();
    let email = field(&fields, "person.email");
    let FieldKind::Text { align, da, .. } = &email.kind else {
        panic!("{email:?}")
    };
    assert_eq!((*align, da.size, &da.font[..]), (2, 10.0, &b"Helv"[..]));
    assert_eq!(email.rect, Rect::from_corners((220.0, 40.0), (380.0, 60.0)));
    let FieldKind::Text { multiline, da, .. } = &field(&fields, "address").kind else {
        panic!()
    };
    assert!(*multiline && da.size == 0.0, "the form's /DA");
    let FieldKind::Text { comb, max_len, .. } = &field(&fields, "code").kind else {
        panic!()
    };
    assert!(*comb && *max_len == Some(5));
    assert!(field(&fields, "amount").read_only);
    assert_eq!(field(&fields, "name").tip.as_deref(), Some("Your name"));
    let FieldKind::Choice { combo, options, .. } = &field(&fields, "country").kind else {
        panic!()
    };
    assert!(*combo && options[1] == ("fr".to_string(), "France".to_string()));
}

/// Whether the check box or radio button `name` on `page` of `doc` reads as on, the first of
/// its widgets there.
fn on(doc: &PdfDoc, page: usize, name: &str) -> Vec<bool> {
    (doc.fields(page).unwrap().iter())
        .filter(|f| f.name == name)
        .map(|f| match f.kind {
            FieldKind::Check { checked, .. } | FieldKind::Radio { checked, .. } => checked,
            _ => panic!("{f:?}"),
        })
        .collect()
}

/// The field `num`'s `/V` and its widget `num`'s `/AS` as the session now has them.
fn value_and_state(doc: &PdfDoc, field: u32, widget: u32) -> (Object, Object) {
    let get = |num: u32, key: &[u8]| {
        let dict = doc
            .cos
            .dict(accent_pdf::Ref { num, generation: 0 })
            .unwrap();
        dict.get(key).cloned().unwrap_or(Object::Null)
    };
    (get(field, b"V"), get(widget, b"AS"))
}

/// A check box filled in writes its on-state as `/V` and `/AS`, draws its cross, keeps it
/// through a save as pdfium reads it, and Undo and Redo walk it off and on again.
#[test]
fn a_check_box_is_ticked_saved_and_walked_back() {
    let Some((dir, mut doc)) = open_pdf(&form_pdf(|o| o)) else {
        return;
    };
    let mut ink = Ink::default();
    let agree = field(&doc.fields(0).unwrap(), "agree").clone();
    let yes = FieldValue::On(Some(b"Yes".to_vec()));
    let areas = ink.fill(&mut doc, agree.field, &yes).unwrap();
    assert_eq!(areas, [(0, agree.rect)]);
    let named = |n: &[u8]| Object::Name(n.to_vec());
    assert_eq!(
        value_and_state(&doc, 15, 15),
        (named(b"Yes"), named(b"Yes"))
    );
    assert_eq!(on(&doc, 0, "agree"), [true]);
    assert!(painted(&doc, 0, agree.rect, red));
    let back = super::reopen(&dir, &doc);
    assert_eq!(on(&back, 0, "agree"), [true]);
    {
        let _guard = lock();
        let page = back.doc().pages().get(0).unwrap();
        let annots = page.annotations();
        let checked = (annots.iter())
            .filter_map(|a| a.as_form_field()?.as_checkbox_field()?.is_checked().ok())
            .collect::<Vec<_>>();
        assert_eq!(checked, [true], "as pdfium reads it");
    }
    let walked = ink.walk(&mut doc, false);
    assert_eq!(walked, [Walked::Ink(0, agree.rect)]);
    assert_eq!(
        value_and_state(&doc, 15, 15),
        (named(b"Off"), named(b"Off"))
    );
    assert!(!painted(&doc, 0, agree.rect, red));
    ink.walk(&mut doc, true);
    assert_eq!(on(&doc, 0, "agree"), [true]);
    assert!(painted(&doc, 0, agree.rect, red));
}

/// A radio group turned on in one state shows that button alone, on whichever page, its other
/// buttons off on both of theirs; turned off, none.
#[test]
fn a_radio_group_turns_one_button_on_across_its_pages() {
    let Some((_dir, mut doc)) = open_pdf(&form_pdf(|o| o)) else {
        return;
    };
    let size = field(&doc.fields(1).unwrap(), "size").clone();
    let filled = doc
        .fill(size.field, &FieldValue::On(Some(b"L".to_vec())))
        .unwrap();
    let pages: Vec<usize> = filled.areas.iter().map(|(page, _)| *page).collect();
    assert_eq!(pages, [0, 1]);
    assert_eq!(filled.kept.len(), 4, "the field and its three widgets");
    assert_eq!(
        (on(&doc, 0, "size"), on(&doc, 1, "size")),
        (vec![false, false], vec![true])
    );
    assert_eq!(value_and_state(&doc, 18, 21).0, Object::Name(b"L".to_vec()));
    assert!(!painted(
        &doc,
        0,
        field(&doc.fields(0).unwrap(), "size").rect,
        green
    ));
    doc.fill(size.field, &FieldValue::On(None)).unwrap();
    assert_eq!(
        (on(&doc, 0, "size"), on(&doc, 1, "size")),
        (vec![false, false], vec![false])
    );
    assert_eq!(
        value_and_state(&doc, 18, 21).0,
        Object::Name(b"Off".to_vec())
    );
}

/// A check box of an encrypted document is filled in through its handler: drawn, and read back
/// from the saved file, whose new objects are never in the clear.
#[test]
fn an_encrypted_check_box_is_filled_in() {
    use super::super::cos::{dict, name, reals};
    let base = accent_pdf::fixtures::encrypted_document(6);
    let pdf = accent_pdf::Pdf::open(std::sync::Arc::new(base.clone())).unwrap();
    let mut update = accent_pdf::Update::new(&pdf);
    let square = |content: &[u8]| {
        let kind = [
            (&b"Type"[..], name(b"XObject")),
            (b"Subtype", name(b"Form")),
        ];
        let mut d = dict(kind);
        d.set(b"BBox", reals([0.0, 0.0, 16.0, 16.0]));
        Object::Stream(accent_pdf::Stream {
            dict: d,
            raw: content.to_vec(),
        })
    };
    let yes = update.add(square(b"1 0 0 RG 2 w 2 2 m 14 14 l 2 14 m 14 2 l S"));
    let off = update.add(square(b""));
    let states = dict([(b"Yes", Object::Ref(yes)), (b"Off", Object::Ref(off))]);
    let widget = update.add(Object::Dict(dict([
        (b"Type", name(b"Annot")),
        (b"Subtype", name(b"Widget")),
        (b"FT", name(b"Btn")),
        (b"T", Object::String(b"agreement".to_vec())),
        (b"V", name(b"Off")),
        (b"AS", name(b"Off")),
        (b"F", Object::Int(4)),
        (b"Rect", reals([20.0, 40.0, 36.0, 56.0])),
        (b"AP", Object::Dict(dict([(b"N", Object::Dict(states))]))),
    ])));
    let r = |num| accent_pdf::Ref { num, generation: 0 };
    let mut page = pdf.get(r(3)).unwrap().as_dict().cloned().unwrap();
    page.set(b"Annots", Object::Array(vec![Object::Ref(widget)]));
    update.set(r(3), Object::Dict(page));
    let mut catalog = pdf.get(r(1)).unwrap().as_dict().cloned().unwrap();
    let fields = Object::Array(vec![Object::Ref(widget)]);
    catalog.set(b"AcroForm", Object::Dict(dict([(b"Fields", fields)])));
    update.set(r(1), Object::Dict(catalog));
    let Some((dir, mut doc)) = open_pdf(&[base, update.section()].concat()) else {
        return;
    };
    let agree = field(&doc.fields(0).unwrap(), "agreement").clone();
    doc.fill(agree.field, &FieldValue::On(Some(b"Yes".to_vec())))
        .unwrap();
    assert!(painted(&doc, 0, agree.rect, red));
    let saved = doc.save().unwrap();
    assert_eq!(memchr::memmem::find(&saved, b"agreement"), None);
    assert_eq!(on(&super::reopen(&dir, &doc), 0, "agreement"), [true]);
}
