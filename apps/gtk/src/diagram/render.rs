//! A diagram off the canvas: a page painted as the canvas paints it (`paint::prim`), in the
//! file's own colours on its own page colour (`Tint::FILE`) whatever the theme on screen, into a
//! render node that cairo draws. Export as PDF…, PNG… and SVG…, Print… and a note's embed all
//! come through here (DESIGN.md, Diagram): fills, lines and text stay vectors in a PDF or an
//! SVG, and a formula or a picture goes in as the image the canvas paints.

use std::rc::Rc;
use std::time::{Duration, Instant};

use accent_drawio::{Color, File, Rect};
use gtk::prelude::*;
use gtk::{cairo, glib, gsk};

use super::geometry::Frame;
use super::math::Typesetter;
use super::paint::{self, Cache, Tint};

/// Points per page unit: draw.io's unit is a CSS pixel, 1/96 in, and a PDF's point 1/72 in, so
/// a Letter page of 850 × 1100 is a Letter sheet.
const PDF_POINT: f64 = 0.75;

/// Pixels per page unit in an exported PNG: sharp on a display at twice the scale.
const PNG_SCALE: f64 = 2.0;

/// Content units per page unit a page is painted at, before cairo takes it to its size: GSK's
/// cairo path draws a scaled picture through an image of that many pixels per page unit, so a
/// formula goes in at the detail it is typeset at and a picture at no less, whatever the
/// output's own scale.
const DETAIL: f64 = super::math::RENDER_ZOOM;

/// How long a page's formulas may take to be typeset before it goes out with their source.
const PATIENCE: Duration = Duration::from_secs(30);

/// What of a page goes out, as draw.io's export takes it by default.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Area {
    /// The page's sheet, grown to hold whatever is drawn past it, as Export as PDF… and Print…
    /// take it; a page with no sheet (`page="0"`) is its drawing alone.
    Sheet,
    /// The drawing alone, as Export as PNG…, Export as SVG… and an embed take it.
    Drawing,
}

/// A page painted: the picture at [`DETAIL`], the part of it that goes out, and its paper.
pub struct Drawn {
    node: Option<gsk::RenderNode>,
    /// In page units, whole ones.
    pub area: Rect,
    paper: Color,
}

impl Drawn {
    /// The area onto `cr` at `scale` per page unit, its top-left at the origin, on its paper.
    pub fn paint(&self, cr: &cairo::Context, scale: f64) -> Result<(), cairo::Error> {
        let (a, p) = (self.area, self.paper);
        cr.save()?;
        cr.scale(scale, scale);
        cr.translate(-a.x, -a.y);
        let unit = |v: u8| f64::from(v) / 255.0;
        cr.set_source_rgba(unit(p.r), unit(p.g), unit(p.b), unit(p.a));
        cr.rectangle(a.x, a.y, a.w, a.h);
        cr.fill()?;
        if let Some(node) = &self.node {
            cr.scale(1.0 / DETAIL, 1.0 / DETAIL);
            node.draw(cr);
        }
        cr.restore()
    }
}

/// Page `i` of `file`, its formulas typeset by `typesetter` first. `None` for no such page.
pub async fn draw(
    file: &File,
    i: usize,
    area: Area,
    typesetter: Option<&Rc<Typesetter>>,
) -> Option<Drawn> {
    let page = file.pages.get(i)?;
    let scene = accent_drawio::scene_with(page, &super::shown(i, file.pages.len()));
    // Pango lays labels out through a widget; their sizes are absolute, so any one does.
    let widget = gtk::DrawingArea::new();
    let cache = Cache::default();
    let frame = Frame {
        scale: DETAIL,
        ..Frame::default()
    };
    let paint = || {
        let snapshot = gtk::Snapshot::new();
        for (j, prim) in scene.prims.iter().enumerate() {
            paint::prim(
                &snapshot,
                widget.upcast_ref(),
                j,
                prim,
                &frame,
                &cache,
                typesetter,
                Tint::FILE,
            );
        }
        snapshot.to_node()
    };
    let mut node = paint();
    // A formula painted as its source has just been asked for: painted again once it is in.
    if let Some(t) = typesetter.filter(|t| t.busy()) {
        let started = Instant::now();
        while t.busy() && started.elapsed() < PATIENCE {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        node = paint();
    }
    let (w, h) = page.size();
    let sheet = page.page_view().then(|| Rect::new(0.0, 0.0, w, h));
    // The node's bounds are what was painted: labels past their boxes, shadows, a line's width.
    let drawing = node.as_ref().map(|n| {
        let b = n.bounds();
        let at = |v: f32| f64::from(v) / DETAIL;
        let (x, y) = (at(b.x()).floor(), at(b.y()).floor());
        let (right, bottom) = (at(b.x() + b.width()).ceil(), at(b.y() + b.height()).ceil());
        Rect::new(x, y, right - x, bottom - y)
    });
    let area = match (area, sheet, drawing) {
        (Area::Sheet, Some(s), Some(d)) => s.union(&d),
        (Area::Sheet, Some(s), None) => s,
        (_, _, Some(d)) => d,
        (_, _, None) => Rect::new(0.0, 0.0, w, h),
    };
    Some(Drawn {
        node,
        area,
        paper: scene.background.unwrap_or(Color::WHITE),
    })
}

/// Every page of `file` as a PDF, a PDF page the size of each sheet.
pub async fn pdf(file: &File, typesetter: Option<&Rc<Typesetter>>) -> Result<Vec<u8>, String> {
    let surface = cairo::PdfSurface::for_stream(1.0, 1.0, Vec::<u8>::new()).map_err(say)?;
    for i in 0..file.pages.len() {
        let Some(drawn) = draw(file, i, Area::Sheet, typesetter).await else {
            continue;
        };
        let (w, h) = (drawn.area.w * PDF_POINT, drawn.area.h * PDF_POINT);
        surface.set_size(w, h).map_err(say)?;
        let cr = cairo::Context::new(&surface).map_err(say)?;
        drawn.paint(&cr, PDF_POINT).map_err(say)?;
        cr.show_page().map_err(say)?;
    }
    bytes_of(surface.finish_output_stream())
}

/// Page `i` of `file` as an SVG of `area`, a page unit to a pixel.
pub async fn svg(
    file: &File,
    i: usize,
    area: Area,
    typesetter: Option<&Rc<Typesetter>>,
) -> Result<Vec<u8>, String> {
    let drawn = draw(file, i, area, typesetter)
        .await
        .ok_or("there is no such page")?;
    let mut surface =
        cairo::SvgSurface::for_stream(drawn.area.w, drawn.area.h, Vec::<u8>::new()).map_err(say)?;
    surface.set_document_unit(cairo::SvgUnit::Px);
    let cr = cairo::Context::new(&surface).map_err(say)?;
    drawn.paint(&cr, 1.0).map_err(say)?;
    drop(cr);
    bytes_of(surface.finish_output_stream())
}

/// Page `i` of `file`, its drawing, as a PNG at [`PNG_SCALE`] pixels per page unit.
pub async fn png(
    file: &File,
    i: usize,
    typesetter: Option<&Rc<Typesetter>>,
) -> Result<Vec<u8>, String> {
    let drawn = draw(file, i, Area::Drawing, typesetter)
        .await
        .ok_or("there is no such page")?;
    let (w, h) = (drawn.area.w * PNG_SCALE, drawn.area.h * PNG_SCALE);
    let surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32).map_err(say)?;
    let cr = cairo::Context::new(&surface).map_err(say)?;
    drawn.paint(&cr, PNG_SCALE).map_err(say)?;
    drop(cr);
    let mut bytes = Vec::new();
    surface.write_to_png(&mut bytes).map_err(say)?;
    Ok(bytes)
}

/// How an `area` big page goes onto `paper`: the scale that fills it, aspect kept, and where its
/// top-left lands for it to be centred.
pub fn fit(area: (f64, f64), paper: (f64, f64)) -> (f64, f64, f64) {
    let scale = (paper.0 / area.0.max(1.0)).min(paper.1 / area.1.max(1.0));
    let (dx, dy) = (
        (paper.0 - area.0 * scale) / 2.0,
        (paper.1 - area.1 * scale) / 2.0,
    );
    (scale, dx, dy)
}

/// The name an export of diagram `key` is offered under, after draw.io's: the diagram's name
/// without its extension, and `page`'s after a hyphen when there is one to tell the file's pages
/// apart by.
pub fn export_name(key: &str, page: Option<&str>, extension: &str) -> String {
    let name = accent_core::path::basename(key);
    let lower = name.to_ascii_lowercase();
    let stem = [".drawio.xml", ".drawio", ".dio"]
        .iter()
        .find(|e| lower.ends_with(*e))
        .map_or(name, |e| &name[..name.len() - e.len()]);
    match page {
        // A page's name may hold a slash, which would make a folder of it.
        Some(page) => format!("{stem}-{}.{extension}", page.replace('/', "-")),
        None => format!("{stem}.{extension}"),
    }
}

fn say(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// What a stream surface wrote into its `Vec`.
fn bytes_of(
    finished: Result<Box<dyn std::any::Any>, cairo::StreamWithError>,
) -> Result<Vec<u8>, String> {
    let stream = finished.map_err(|e| e.error.to_string())?;
    stream
        .downcast::<Vec<u8>>()
        .map(|bytes| *bytes)
        .map_err(|_| "the surface wrote nowhere".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_fitted_to_the_paper_and_centred() {
        assert_eq!(fit((100.0, 50.0), (200.0, 200.0)), (2.0, 0.0, 50.0));
        assert_eq!(fit((850.0, 1100.0), (425.0, 1100.0)), (0.5, 0.0, 275.0));
    }

    #[test]
    fn an_export_is_named_after_the_diagram_and_its_page() {
        assert_eq!(export_name("Figures/flow.drawio", None, "pdf"), "flow.pdf");
        assert_eq!(
            export_name("flow.drawio.xml", Some("Page-2"), "png"),
            "flow-Page-2.png"
        );
        assert_eq!(export_name("a.DIO", Some("in/out"), "svg"), "a-in-out.svg");
    }
}
