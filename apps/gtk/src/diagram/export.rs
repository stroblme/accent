//! Export as PDF…, PNG… and SVG…, and Print…, for a diagram: the pages as the tab holds them,
//! unsaved edits and all, drawn by [`render`] in the file's own colours. A PDF has every page,
//! a PNG and an SVG the page on screen, and a printout every page, one sheet each.

use std::path::PathBuf;
use std::rc::Rc;

use accent_drawio::File;
use gtk::prelude::*;
use gtk::{gio, glib};

use super::DiagramTab;
use super::math::Typesetter;
use super::render::{self, Area, Drawn};
use crate::App;
use crate::export::Export;

/// The name Export as … offers for `tab`, `None` for what a diagram is not exported as.
pub fn offered_name(tab: &DiagramTab, to: Export) -> Option<String> {
    let (key, file) = (tab.key(), tab.file());
    // Only a page of several is told apart by its name, and only one that has a name.
    let page = file
        .pages
        .get(tab.page_index())
        .map(|p| p.name())
        .filter(|name| file.pages.len() > 1 && !name.is_empty());
    match to {
        Export::Pdf => Some(render::export_name(&key, None, "pdf")),
        Export::Png => Some(render::export_name(&key, page, "png")),
        Export::Svg => Some(render::export_name(&key, page, "svg")),
        Export::Html => None,
    }
}

/// Export as … once the destination is chosen: drawn here, written to `dest` on a worker
/// through the atomic save, and a toast.
pub fn export_to(app: &Rc<App>, tab: &Rc<DiagramTab>, to: Export, dest: PathBuf) {
    let name = crate::export::dest_name(&dest);
    let (file, page, typesetter) = (tab.file(), tab.page_index(), tab.typesetter());
    let web = tab.web_allowed();
    let done = format!("Exported {name}");
    app.busy_with(
        format!("Exporting {name}…"),
        format!("export {name}"),
        async move {
            let typesetter = typesetter.as_ref();
            let bytes = match to {
                Export::Pdf => render::pdf(&file, typesetter, web).await,
                Export::Png => render::png(&file, page, typesetter, web).await,
                Export::Svg => render::svg(&file, page, Area::Drawing, typesetter, web).await,
                Export::Html => return Ok(None),
            }?;
            let write = move || accent_core::fs::write_bytes(&dest, &bytes, None);
            match crate::work::off_thread("export", write).await {
                Some(Ok(_)) => Ok(Some(done)),
                Some(Err(e)) => Err(e.to_string()),
                None => Err("the worker stopped".to_string()),
            }
        },
    );
}

/// Print…: every page through GTK's print dialog, each fitted to a sheet of the paper chosen.
pub fn print(app: &Rc<App>, tab: &Rc<DiagramTab>) {
    let name = crate::doc::file_name(&tab.key()).to_string();
    let (file, typesetter, window) = (tab.file(), tab.typesetter(), app.window.clone());
    let web = tab.web_allowed();
    app.busy_with(
        format!("Printing {name}…"),
        format!("print {name}"),
        async move {
            let op = operation(&file, typesetter.as_ref(), web, &name).await;
            // From an idle, as a note's print: the dialog runs a main loop of its own, and glib
            // aborts on a future polled inside another's poll.
            let ran = gio::GioFuture::new(&op, move |op, _, done| {
                let op = op.clone();
                glib::idle_add_local_once(move || {
                    done.resolve(op.run(gtk::PrintOperationAction::PrintDialog, Some(&window)))
                });
            })
            .await;
            ran.map(|_| None).map_err(|e| e.to_string())
        },
    );
}

/// A print of every page of `file`: one sheet each, the page's sheet fitted to the paper inside
/// a note's margins, aspect kept and centred, the paper turned to the page's orientation.
pub async fn operation(
    file: &File,
    typesetter: Option<&Rc<Typesetter>>,
    web: bool,
    name: &str,
) -> gtk::PrintOperation {
    let mut pages: Vec<Drawn> = Vec::new();
    for i in 0..file.pages.len() {
        pages.extend(render::draw(file, i, Area::Sheet, typesetter, web).await);
    }
    let pages = Rc::new(pages);
    let op = gtk::PrintOperation::new();
    op.set_job_name(name);
    op.set_n_pages(pages.len() as i32);
    op.set_default_page_setup(Some(&crate::export::page_setup()));
    op.connect_request_page_setup(glib::clone!(
        #[strong]
        pages,
        move |_, _, i, setup| {
            if let Some(drawn) = pages.get(i as usize) {
                setup.set_orientation(match drawn.area.w > drawn.area.h {
                    true => gtk::PageOrientation::Landscape,
                    false => gtk::PageOrientation::Portrait,
                });
            }
        }
    ));
    op.connect_draw_page(move |_, context, i| {
        let Some(drawn) = pages.get(i as usize) else {
            return;
        };
        let cr = context.cairo_context();
        let (area, paper) = (
            (drawn.area.w, drawn.area.h),
            (context.width(), context.height()),
        );
        let (scale, dx, dy) = render::fit(area, paper);
        cr.translate(dx, dy);
        if let Err(e) = drawn.paint(&cr, scale) {
            tracing::warn!("diagram page {i} not printed: {e}");
        }
    });
    op
}
