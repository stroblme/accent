//! Form fields: what accent reads of them against what pdfium reads, and what pdfium draws of a
//! widget, from its appearance stream or from its value alone.

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
