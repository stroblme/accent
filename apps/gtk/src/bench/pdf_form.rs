//! The form drill, `ACCENT_BENCH_PDF=form`: `accent_pdf`'s fixture form written into the vault
//! as `form-drill.pdf`, the fields of its first page as the tab reads them, and through XTEST the
//! pointer over each with the tooltip a reader is shown, the page pictured as `form.png` under
//! `ACCENT_BENCH_SHOTS=<dir>`; then a check box ticked and a radio button picked by real clicks,
//! and the pick undone, each against the file. Takes the file away again.

use super::pdf::{opened, written};
use super::*;
use accent_core::pdf::{FieldKind, FormField};

/// What a field is, in a word.
fn kind_of(field: &FormField) -> &'static str {
    match &field.kind {
        FieldKind::Text { password: true, .. } => "password",
        FieldKind::Text { comb: true, .. } => "comb",
        FieldKind::Text {
            multiline: true, ..
        } => "multiline",
        FieldKind::Text { .. } => "text",
        FieldKind::Check { .. } => "check",
        FieldKind::Radio { .. } => "radio",
        FieldKind::Choice { combo: true, .. } => "combo",
        FieldKind::Choice { .. } => "list",
        FieldKind::Button => "button",
        FieldKind::Signature => "signature",
    }
}

const NAME: &str = "form-drill.pdf";

/// The page `view` is in as it is on screen, an editor over it included, as `name` under
/// `ACCENT_BENCH_SHOTS`.
fn shot(view: &gtk::Widget, name: &str) {
    let Ok(dir) = std::env::var("ACCENT_BENCH_SHOTS") else {
        return;
    };
    if let Some(overlay) = view.parent().and_then(|scroller| scroller.parent()) {
        super::diagram::shoot(&overlay, &std::path::Path::new(&dir).join(name));
    }
}

pub(super) fn bench_pdf_form(app: &Rc<App>) {
    let path = app.root().join(NAME);
    if let Err(e) = std::fs::write(&path, accent_pdf::fixtures::form_document()) {
        println!("bench form wrote=false {e}");
        return bench_quit(app);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, NAME).await else {
            println!("bench form no_tab");
            return quit(&app, &path);
        };
        let view = pdf.views()[0].clone();
        let Some(reader) = view.downcast_ref::<pdfview::PdfView>().cloned() else {
            return quit(&app, &path);
        };
        let mut fields = None;
        for _ in 0..100 {
            fields = pdf.fields_heard(0);
            if fields.is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let Some(fields) = fields else {
            println!("bench form no_fields");
            return quit(&app, &path);
        };
        let kinds: Vec<String> = (fields.iter())
            .map(|f| match f.read_only {
                true => format!("{}:{}(read-only)", f.name, kind_of(f)),
                false => format!("{}:{}", f.name, kind_of(f)),
            })
            .collect();
        println!(
            "bench form fields page=0 count={} {}",
            fields.len(),
            kinds.join(" ")
        );

        let (sx, sy) = app.window.surface_transform();
        // The middle of a field in the view, and that point on the screen, which is where XTEST
        // aims.
        let middle = |f: &FormField| {
            let r = reader.widget_rect(0, &f.rect)?;
            Some((
                f64::from(r.x()) + f64::from(r.width()) / 2.0,
                f64::from(r.y()) + f64::from(r.height()) / 2.0,
            ))
        };
        let on_screen = |(x, y): (f64, f64)| {
            let p = graphene::Point::new(x as f32, y as f32);
            let p = view.compute_point(&app.window, &p).unwrap_or(p);
            (f64::from(p.x()) + sx, f64::from(p.y()) + sy)
        };
        super::git::xtest("focus").await;
        let mut cursors = Vec::new();
        for f in &fields {
            let Some(at) = middle(f) else { continue };
            let (x, y) = on_screen(at);
            super::git::xtest(&format!("move {x:.0} {y:.0}; sleep 0.1")).await;
            let cursor = reader.cursor().and_then(|c| c.name()).unwrap_or_default();
            cursors.push(format!("{}={cursor}", f.name));
        }
        println!("bench form cursor {}", cursors.join(" "));
        let tips: Vec<String> = (fields.iter())
            .filter_map(|f| Some((f, middle(f)?)))
            .filter_map(|(f, (x, y))| Some(format!("{}={:?}", f.name, pdf.field_tip_at(x, y)?)))
            .collect();
        println!("bench form tips {}", tips.join(" "));
        shot(&view, "form.png");

        let click = async |f: &FormField| {
            if let Some((x, y)) = middle(f).map(on_screen) {
                super::git::xtest(&format!("move {x:.0} {y:.0}; down; up; sleep 0.3")).await;
            }
        };
        let save = async || {
            pdf.flush();
            written(&app).await;
        };
        let named = |name: &str| fields.iter().find(|f| f.name == name).cloned();
        let (Some(agree), Some(small)) = (named("agree"), named("size")) else {
            println!("bench form unnamed");
            return quit(&app, &path);
        };
        click(&agree).await;
        save().await;
        println!("bench form ticked {}", filed(&path));
        click(&small).await;
        click(&small).await;
        save().await;
        println!("bench form picked {}", filed(&path));
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-undo", None);
        save().await;
        println!("bench form undone {}", filed(&path));
        quit(&app, &path);
    });
}

/// The check boxes, radio buttons and text fields of the first page of `path` as its file says
/// them, in `/Annots` order: `name=value`.
fn filed(path: &std::path::Path) -> String {
    let Ok(doc) = accent_core::pdf::PdfDoc::open(path) else {
        return "unreadable".to_string();
    };
    let fields = doc.fields(0).unwrap_or_default();
    let said: Vec<String> = (fields.iter())
        .filter_map(|f| {
            let value = match &f.kind {
                FieldKind::Check { checked, .. } => checked.to_string(),
                FieldKind::Radio { checked, .. } => checked.to_string(),
                FieldKind::Text { value, .. } if !value.is_empty() => format!("{value:?}"),
                _ => return None,
            };
            Some(format!("{}={value}", f.name))
        })
        .collect();
    said.join(" ")
}

fn quit(app: &Rc<App>, path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    bench_quit(app);
}
