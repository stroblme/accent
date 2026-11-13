//! Probe diagram edit/save round trips and canvas rendering.
//! This module reads the mode and holds what the drills share, one child module per drill area.

use super::*;

mod drag;
mod input;
mod layers;
mod output;
mod pictures;
mod props;
mod round;

/// A two-page diagram with a shape, an ellipse, an edge between them, a formula, a label of text
/// and formulas, and a bracket and a text turned a quarter each way.
pub(super) const SAMPLE: &str = r#"<mxfile host="accent">
  <diagram name="One" id="bench-one">
    <mxGraphModel grid="1" gridSize="10" page="1" pageWidth="800" pageHeight="500" math="1">
      <root>
        <mxCell id="0" />
        <mxCell id="1" parent="0" />
        <mxCell id="a" value="A" style="rounded=1;whiteSpace=wrap;html=1;fillColor=#dae8fc;strokeColor=#6c8ebf;" parent="1" vertex="1">
          <mxGeometry x="100" y="100" width="120" height="60" as="geometry" />
        </mxCell>
        <mxCell id="b" value="&lt;b&gt;B&lt;/b&gt;" style="ellipse;whiteSpace=wrap;html=1;" parent="1" vertex="1">
          <mxGeometry x="400" y="100" width="120" height="60" as="geometry" />
        </mxCell>
        <mxCell id="e" style="edgeStyle=orthogonalEdgeStyle;rounded=0;html=1;" parent="1" source="a" target="b" edge="1">
          <mxGeometry relative="1" as="geometry" />
        </mxCell>
        <mxCell id="m" value="\(x^2\)" style="text;html=1;" parent="1" vertex="1">
          <mxGeometry x="100" y="300" width="80" height="30" as="geometry" />
        </mxCell>
        <mxCell id="f" value="Area \(\pi r^2\) and $$\frac{a}{b}$$" style="text;html=1;whiteSpace=wrap;align=center;" parent="1" vertex="1">
          <mxGeometry x="100" y="380" width="200" height="80" as="geometry" />
        </mxCell>
        <mxCell id="k" style="shape=curlyBracket;whiteSpace=wrap;html=1;rounded=1;rotation=90;" parent="1" vertex="1">
          <mxGeometry x="350" y="260" width="20" height="160" as="geometry" />
        </mxCell>
        <mxCell id="t" value="Turned" style="text;html=1;align=center;verticalAlign=middle;rotation=-90;" parent="1" vertex="1">
          <mxGeometry x="560" y="330" width="120" height="30" as="geometry" />
        </mxCell>
      </root>
    </mxGraphModel>
  </diagram>
  <diagram name="Two" id="bench-two">
    <mxGraphModel><root><mxCell id="0" /><mxCell id="1" parent="0" /></root></mxGraphModel>
  </diagram>
</mxfile>
"#;

/// `ACCENT_BENCH_DIAGRAM=<rel>` edits a diagram, creating a sample if absent.
/// Modes:
///
/// - `=shot:<rel>:<dir>` renders `<dir>/page-N.png` and prints per-page time.
/// - `=hold:<rel>[:<tool>]` reports screen targets, waits 10 s for XTEST input, then
///   prints the model.
/// - `=preview:<rel>` times no-drag, most-connected-shape live drag and whole-page
///   box drag on the largest page.
/// - `=strip:<rel>` holds the Outline pane's page thumbnails on screen for XTEST to hover,
///   click and drag along, printing the pages and the strip's buttons every two seconds.
/// - `=present:<rel>,<pdf>`, `=look:<rel>:<dir>`, `=export:<rel>:<dir>`, `=props:<rel>`,
///   `=layers:<rel>` and `=web:<rel>:<url>` probe presentation, recolouring, output,
///   Properties, Layers and web images through the corresponding helpers.
///
/// `ACCENT_BENCH_DIAGRAM_SHOT=<png>` adds a picture of the window to the plain round, `props:`,
/// `layers:` and `hold:`, for looking at.
pub(super) fn bench_diagram(app: &Rc<App>, arg: &str) {
    if let Some((rel, url)) = arg.strip_prefix("web:").and_then(|a| a.split_once(':')) {
        return pictures::web(app, rel, url);
    }
    if let Some(rel) = arg.strip_prefix("sharp:") {
        return pictures::sharp(app, rel);
    }
    if let Some(rel) = arg.strip_prefix("layers:") {
        return layers::layers(app, rel);
    }
    if let Some(rel) = arg.strip_prefix("preview:") {
        return drag::preview(app, rel);
    }
    if let Some(rel) = arg.strip_prefix("strip:") {
        return input::strip(app, rel);
    }
    if let Some(rel) = arg.strip_prefix("props:") {
        return props::props(app, rel);
    }
    if let Some((rel, dir)) = arg.strip_prefix("export:").and_then(|a| a.split_once(':')) {
        return output::export(app, rel, Path::new(dir));
    }
    if let Some((rel, dir)) = arg.strip_prefix("look:").and_then(|a| a.split_once(':')) {
        return output::look(app, rel, Path::new(dir));
    }
    if let Some((rel, pdf)) = arg.strip_prefix("present:").and_then(|a| a.split_once(',')) {
        return input::present(app, rel, pdf);
    }
    if let Some(rest) = arg.strip_prefix("hold:") {
        let (rel, tool) = rest.split_once(':').unwrap_or((rest, ""));
        return input::hold(app, rel, tool);
    }
    let (rel, shots) = match arg.strip_prefix("shot:") {
        Some(rest) => match rest.split_once(':') {
            Some((rel, dir)) => (rel.to_string(), Some(PathBuf::from(dir))),
            None => (rest.to_string(), None),
        },
        None => (arg.to_string(), None),
    };
    let path = app.root().join(&rel);
    let written = shots.is_none() && !path.exists();
    if written {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let app = app.clone();
    // A file written a moment ago is only in the vault once the watcher has seen it.
    let wait = if written { 1500 } else { 0 };
    glib::timeout_add_local_once(Duration::from_millis(wait), move || {
        app.open_path(&rel);
        glib::timeout_add_local_once(Duration::from_millis(800), move || {
            round::drill(&app, shots)
        });
    });
}

/// Paint `widget` as it is on screen into a PNG at `path`.
pub(super) fn shoot(widget: &gtk::Widget, path: &Path) -> bool {
    picture(widget).is_some_and(|t| t.save_to_png(path).is_ok())
}

/// `widget` as it is on screen, painted by its window's renderer.
fn picture(widget: &gtk::Widget) -> Option<gdk::Texture> {
    let (w, h) = (f64::from(widget.width()), f64::from(widget.height()));
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w, h);
    let node = snapshot.to_node()?;
    let renderer = widget.native()?.renderer()?;
    let viewport = graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
    Some(renderer.render_texture(&node, Some(&viewport)))
}

/// Where `(x, y)` of `widget` is on the screen, for XTEST: the window sits at 0,0 with no window
/// manager, its client-side shadow putting the widgets a margin in from the surface's corner.
pub(super) fn on_screen(
    app: &App,
    widget: &impl IsA<gtk::Widget>,
    x: f64,
    y: f64,
) -> Option<String> {
    let p = widget.compute_point(&app.window, &graphene::Point::new(x as f32, y as f32))?;
    let (sx, sy) = app.window.surface_transform();
    Some(format!(
        "{} {}",
        f64::from(p.x()) + sx,
        f64::from(p.y()) + sy
    ))
}

/// Where the middle of `widget` is on the screen.
pub(super) fn middle_on_screen(app: &App, widget: &impl IsA<gtk::Widget>) -> Option<String> {
    let (w, h) = (f64::from(widget.width()), f64::from(widget.height()));
    on_screen(app, widget, w / 2.0, h / 2.0)
}
