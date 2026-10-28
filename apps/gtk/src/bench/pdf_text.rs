//! The Text tool's drill, `ACCENT_BENCH_PDF=text:<rel>`: through XTEST, text placed on the
//! page being read, typed and finished each way, typed again, moved with Adjust and erased, with
//! the Undo of each, as the file has it after every step.

use super::pdf::{opened, written};
use super::*;

/// Every text of ours on `page` of the file, as `"what it says"@x,y/size#colour`, in `/Annots`
/// order, then how many comments the page lists, which ours never are.
fn filed(pdf: &pdftab::PdfTab, page: usize) -> String {
    let Ok(doc) = accent_core::pdf::PdfDoc::open(pdf.path()) else {
        return "unreadable".to_string();
    };
    let texts: Vec<String> = (doc.texts(page).unwrap_or_default().iter())
        .map(|t| {
            let ((x, y), [r, g, b]) = (t.typed.at, t.typed.style.rgb);
            let size = t.typed.style.size;
            format!(
                "{:?}@{x:.0},{y:.0}/{size}#{r:02x}{g:02x}{b:02x}",
                t.typed.text
            )
        })
        .collect();
    let comments = doc.comments(page).map_or(0, |c| c.len());
    format!("texts=[{}] comments={comments}", texts.join(" "))
}

/// What the page `view` is in shows, the editor over it included, pictured into `$TMPDIR`.
fn picture(view: &gtk::Widget, name: &str) {
    let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    if let Some(overlay) = view.parent().and_then(|scroller| scroller.parent()) {
        super::diagram::shoot(&overlay, &std::path::Path::new(&dir).join(name));
    }
}

pub(super) fn bench_pdf_text(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench text no_tab");
            return bench_quit(&app);
        };
        let page = pdf.current_page();
        let view = pdf.views()[0].clone();
        let Some(reader) = view.downcast_ref::<pdfview::PdfView>().cloned() else {
            return bench_quit(&app);
        };
        let (sx, sy) = app.window.surface_transform();
        let (w, h) = (f64::from(view.width()), f64::from(view.height()));
        // A point of the view on the screen, which is where XTEST aims.
        let on_screen = |x: f64, y: f64| {
            let p = graphene::Point::new(x as f32, y as f32);
            let p = view.compute_point(&app.window, &p).unwrap_or(p);
            (f64::from(p.x()) + sx, f64::from(p.y()) + sy)
        };
        let click = |(x, y): (f64, f64)| format!("move {x:.0} {y:.0}; down; up; sleep 0.3");
        // The middle of the first text of ours the view holds, in its own coordinates.
        let first = || {
            let texts = reader.texts_heard(page)?;
            let (_, text) = texts.first()?;
            let r = reader.widget_rect(page, &text.bounds)?;
            Some((
                f64::from(r.x()) + f64::from(r.width()) / 2.0,
                f64::from(r.y()) + f64::from(r.height()) / 2.0,
            ))
        };
        let editing = |pdf: &pdftab::PdfTab| match pdf.typing_at() {
            Some((at, content, px, focus)) => format!(
                "open at={:.0},{:.0} size={:.0}x{:.0} content_h={content:.0} px={px:.1} focus={focus}",
                at.x, at.y, at.w, at.h
            ),
            None => "closed".to_string(),
        };
        let save = async || {
            pdf.flush();
            written(&app).await;
        };
        let undo = async |redo: bool| {
            let action = if redo { "win.pdf-redo" } else { "win.pdf-undo" };
            let _ = WidgetExt::activate_action(&app.window, action, None);
            pdf.flush();
            written(&app).await;
        };

        app.pdf_mode(pdfview::Mode::Text);
        glib::timeout_future(Duration::from_secs(1)).await;
        println!("bench text tool {}", pdf.ring().state());
        // Typed with Return for a second line and finished with Ctrl+Return; what Helvetica
        // cannot set refused as it is typed.
        let at = on_screen(w * 0.3, h * 0.3);
        let steps = format!(
            "{}; focus; type Hallo Welt; key Return; type zwei",
            click(at)
        );
        super::git::xtest(&steps).await;
        pdf.type_text(" äö€→");
        glib::timeout_future(Duration::from_millis(300)).await;
        println!("bench text typing {}", editing(&pdf));
        picture(&view, "text-typing.png");
        super::git::xtest("key ctrl+Return").await;
        save().await;
        println!("bench text typed {} {}", editing(&pdf), filed(&pdf, page));
        // Where the editor stood, against where the page sets the text: the text's box.
        glib::timeout_future(Duration::from_secs(1)).await;
        let boxed = reader.texts_heard(page).and_then(|texts| {
            let (_, t) = texts.first()?.clone();
            reader.widget_rect(page, &t.bounds)
        });
        println!("bench text box {boxed:?}");
        picture(&view, "text-typed.png");

        // Typed again: its editor over it, holding it, finished with Escape.
        let Some(middle) = first() else {
            println!("bench text unlisted");
            return bench_quit(&app);
        };
        let steps = format!(
            "{}; key End; key space; type mehr; key Escape",
            click(on_screen(middle.0, middle.1))
        );
        super::git::xtest(&steps).await;
        save().await;
        println!("bench text retyped {} {}", editing(&pdf), filed(&pdf, page));
        undo(false).await;
        println!("bench text retyped_undone {}", filed(&pdf, page));
        undo(true).await;
        println!("bench text retyped_redone {}", filed(&pdf, page));

        // A box left empty writes nothing; a click away finishes a text and opens no other.
        super::git::xtest(&format!(
            "{}; key Escape",
            click(on_screen(w * 0.6, h * 0.6))
        ))
        .await;
        save().await;
        println!("bench text empty {} {}", editing(&pdf), filed(&pdf, page));
        let steps = format!(
            "{}; type x; {}",
            click(on_screen(w * 0.6, h * 0.7)),
            click(on_screen(w * 0.2, h * 0.8))
        );
        super::git::xtest(&steps).await;
        save().await;
        println!(
            "bench text clicked_away {} {}",
            editing(&pdf),
            filed(&pdf, page)
        );

        // Moved by Adjust 40 px down, and back by Undo.
        app.pdf_mode(pdfview::Mode::Adjust);
        glib::timeout_future(Duration::from_secs(1)).await;
        if let Some((x, y)) = first() {
            let (x, y) = on_screen(x, y);
            let steps = format!(
                "move {x:.0} {y:.0}; down; sleep 0.1; move {x:.0} {:.0}; sleep 0.1; \
                 move {x:.0} {:.0}; sleep 0.1; up",
                y + 20.0,
                y + 40.0
            );
            super::git::xtest(&steps).await;
        }
        save().await;
        println!(
            "bench text moved held={:?} {}",
            reader.adjusted().is_some(),
            filed(&pdf, page)
        );
        undo(false).await;
        println!("bench text moved_undone {}", filed(&pdf, page));

        // Erased whole by a pass across it, the partial eraser too, and back by Undo.
        app.pdf_mode(pdfview::Mode::Eraser);
        let mut config = app.config.borrow().drawing.clone();
        config.eraser_partial = true;
        pdf.set_drawing_config(config);
        glib::timeout_future(Duration::from_secs(1)).await;
        if let Some((x, y)) = first() {
            let (from, to) = (on_screen(x - 30.0, y - 30.0), on_screen(x + 30.0, y + 30.0));
            let steps = format!(
                "move {:.0} {:.0}; down; sleep 0.1; move {:.0} {:.0}; sleep 0.1; up",
                from.0, from.1, to.0, to.1
            );
            super::git::xtest(&steps).await;
        }
        save().await;
        println!("bench text erased {}", filed(&pdf, page));
        undo(false).await;
        println!("bench text erased_undone {}", filed(&pdf, page));

        // Typed again into nothing: emptied, it is erased.
        app.pdf_mode(pdfview::Mode::Text);
        glib::timeout_future(Duration::from_secs(1)).await;
        if let Some((x, y)) = first() {
            let steps = format!(
                "{}; key ctrl+a; key Delete; key Escape",
                click(on_screen(x, y))
            );
            super::git::xtest(&steps).await;
        }
        save().await;
        println!("bench text emptied {}", filed(&pdf, page));
        bench_quit(&app);
    });
}
