//! Drills over a draw.io diagram: an edit round trip through the model, the save and the disk,
//! and pictures of every page as the canvas paints them.

use super::*;
use accent_core::config::Theme;

/// A two-page diagram with a shape, an ellipse, an edge between them, a formula, and a bracket
/// and a text turned a quarter each way.
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

/// A page of two layers, as a slide template has them: a locked one with no name holding the
/// template's shape, and "Content" over it with a shape the edge `e` joins to the template's.
const LAYERED: &str = r#"<mxfile host="accent">
  <diagram name="Slide" id="bench-layers">
    <mxGraphModel grid="1" gridSize="10" page="1" pageWidth="800" pageHeight="500">
      <root>
        <mxCell id="0" />
        <mxCell id="1" style="locked=1;" parent="0" />
        <mxCell id="bg" value="Template" style="rounded=0;whiteSpace=wrap;html=1;" parent="1" vertex="1">
          <mxGeometry x="40" y="40" width="200" height="100" as="geometry" />
        </mxCell>
        <mxCell id="2" value="Content" parent="0" />
        <mxCell id="c" value="C" style="ellipse;whiteSpace=wrap;html=1;" parent="2" vertex="1">
          <mxGeometry x="400" y="200" width="120" height="80" as="geometry" />
        </mxCell>
        <mxCell id="e" style="endArrow=classic;html=1;" parent="2" source="bg" target="c" edge="1">
          <mxGeometry relative="1" as="geometry" />
        </mxCell>
      </root>
    </mxGraphModel>
  </diagram>
</mxfile>
"#;

/// `ACCENT_BENCH_DIAGRAM=<rel>` edits the diagram at `rel` (written from a sample first when
/// there is none) and prints each step; `=shot:<rel>:<dir>` paints every page of it into
/// `<dir>/page-N.png` and prints how long each took; `=hold:<rel>[:<tool>]` prints where the
/// sample's shapes are on the screen and stays up for ten seconds, for an XTEST pointer to work
/// on (`build-aux/xtest.py`), then prints what the model holds; `=preview:<rel>` times the
/// frames of a move on the page of `rel` with the most cells: none moving, the shape with the
/// most edges on it moving live, and everything on the page moving as a box;
/// `=present:<rel>,<pdf>` is presentation over both (`present`); `=look:<rel>:<dir>` walks
/// the sample through the themes (`look`); `=export:<rel>:<dir>` exports and prints it into
/// `<dir>` (`export`); `=props:<rel>` works the Properties pane's Position, Size and Style
/// groups (`props`), and `=layers:<rel>` its Layers group over a slide template (`layers`).
pub(super) fn bench_diagram(app: &Rc<App>, arg: &str) {
    if let Some(rel) = arg.strip_prefix("layers:") {
        return layers(app, rel);
    }
    if let Some(rel) = arg.strip_prefix("preview:") {
        return preview(app, rel);
    }
    if let Some(rel) = arg.strip_prefix("props:") {
        return props(app, rel);
    }
    if let Some((rel, dir)) = arg.strip_prefix("export:").and_then(|a| a.split_once(':')) {
        return export(app, rel, Path::new(dir));
    }
    if let Some((rel, dir)) = arg.strip_prefix("look:").and_then(|a| a.split_once(':')) {
        return look(app, rel, Path::new(dir));
    }
    if let Some((rel, pdf)) = arg.strip_prefix("present:").and_then(|a| a.split_once(',')) {
        return present(app, rel, pdf);
    }
    if let Some(rest) = arg.strip_prefix("hold:") {
        let (rel, tool) = rest.split_once(':').unwrap_or((rest, ""));
        return hold(app, rel, tool);
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
        glib::timeout_add_local_once(Duration::from_millis(800), move || drill(&app, shots));
    });
}

fn drill(app: &Rc<App>, shots: Option<PathBuf>) {
    let Some(tab) = app.active_diagram() else {
        println!("bench diagram no_tab");
        return bench_quit(app);
    };
    println!(
        "bench diagram pages={} names={:?} facts={:?}",
        tab.page_count(),
        tab.page_names(),
        tab.facts()
    );
    match shots {
        Some(dir) => {
            shoot_pages(&tab, &dir);
            bench_quit(app);
        }
        None => edit_round(app, &tab),
    }
}

fn shoot_pages(tab: &Rc<crate::diagram::DiagramTab>, dir: &Path) {
    std::fs::create_dir_all(dir).expect("a directory for the pictures");
    for i in 0..tab.page_count() {
        tab.show_page(i);
        bench_pump();
        // Formulas are typeset in WebKit's own time; the picture waits for them.
        let asked = Instant::now();
        while asked.elapsed() < Duration::from_secs(10) {
            glib::MainContext::default().iteration(false);
            if tab.typesetting() {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            // One more frame paints what arrived and asks for what the paint found missing.
            bench_pump();
            if !tab.typesetting() {
                break;
            }
        }
        let started = Instant::now();
        let shot = shoot(&tab.key_target(), &dir.join(format!("page-{}.png", i + 1)));
        println!(
            "bench diagram page={} paint_ms={:.1} {}",
            i + 1,
            ms_since(started),
            if shot { "written" } else { "NOT WRITTEN" }
        );
    }
}

/// Paint `widget` as it is on screen into a PNG at `path`.
fn shoot(widget: &gtk::Widget, path: &Path) -> bool {
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

/// Under Dark, Export as PDF…, PNG… and SVG… of the sample (written to `rel` when there is none)
/// into `<dir>` under the names the chooser offers, as each does once the chooser has answered,
/// and Print… sent to `<dir>/print.pdf` by the print system's Export: printed for each, its pages
/// and their sizes, the PNG's commonest pixel (its paper: white, the file's, not the theme's) and
/// whether the SVG draws; then the SVG a note's `![[rel]]` and `![[rel#Two]]` show, and how long
/// each took.
fn export(app: &Rc<App>, rel: &str, dir: &Path) {
    use crate::diagram::export::{export_to, offered_name, operation};
    use crate::export::Export;
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    std::fs::create_dir_all(dir).expect("a directory for the exports");
    let (app, rel, dir) = (app.clone(), rel.to_string(), dir.to_path_buf());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        crate::theme::apply(Theme::Dark);
        app.restyle_all();
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        for to in [Export::Pdf, Export::Png, Export::Svg] {
            let Some(name) = offered_name(&tab, to) else {
                continue;
            };
            let dest = dir.join(&name);
            let _ = std::fs::remove_file(&dest);
            let started = Instant::now();
            export_to(&app, &tab, to, dest.clone());
            let said = super::export::toast(&app, &format!("Exported {name}")).await;
            println!(
                "bench diagram export {name} ms={:.0} said={said:?} {}",
                ms_since(started),
                exported(&dest)
            );
        }
        let print = dir.join("print.pdf");
        let _ = std::fs::remove_file(&print);
        let op = operation(&tab.file(), tab.typesetter().as_ref(), "print").await;
        op.set_export_filename(&print);
        // From an idle, as Print… runs it: the export runs a main loop of its own.
        let ran = gio::GioFuture::new(&op, |op, _, done| {
            let op = op.clone();
            glib::idle_add_local_once(move || {
                done.resolve(op.run(gtk::PrintOperationAction::Export, None::<&gtk::Window>))
            });
        })
        .await;
        println!("bench diagram print {ran:?} {}", exported(&print));
        // A note's embed: the first page, its formula typeset by the typesetter no tab holds.
        for page in [None, Some("Two")] {
            let started = Instant::now();
            let svg = crate::diagram::embed::svg(&tab.path(), page).await;
            println!(
                "bench diagram embed page={page:?} ms={:.0} bytes={} pictures={}",
                ms_since(started),
                svg.as_ref().map_or(0, |s| s.len()),
                svg.as_ref().map_or(0, |s| s.matches("<image").count())
            );
        }
        // The tab's formulas and the embed's typeset by one view, which is one web process.
        println!("bench diagram web_processes={}", web_processes());
        // A click on one, which follows `rel#Two` as a link: onto that page, or a toast.
        app.open_target(&format!("{rel}#Two"));
        glib::timeout_future(Duration::from_millis(1000)).await;
        println!("bench diagram follow #Two page={}", tab.page_index());
        app.open_target(&format!("{rel}#Nope"));
        let said = super::export::toast(&app, "No page").await;
        println!("bench diagram follow #Nope said={said:?}");
        bench_quit(&app);
    });
}

/// How many WebKit web processes this one has started, a sandbox between or not: read off
/// `/proc`, signalling nothing.
fn web_processes() -> usize {
    let mut procs: HashMap<u32, (u32, String)> = HashMap::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (comm) state ppid …`, the name in brackets that may hold spaces.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let ppid = stat[close + 1..].split_whitespace().nth(1);
        if let Some(ppid) = ppid.and_then(|p| p.parse().ok()) {
            procs.insert(pid, (ppid, stat[open + 1..close].to_string()));
        }
    }
    let me = std::process::id();
    let ours = |mut pid: u32| {
        while let Some(&(parent, _)) = procs.get(&pid) {
            if parent == me {
                return true;
            }
            pid = parent;
        }
        false
    };
    procs
        .iter()
        .filter(|(pid, (_, name))| name.starts_with("WebKitWebProces") && ours(**pid))
        .count()
}

/// What an export wrote: a PDF's page sizes in points, a picture's size and its commonest pixel,
/// and the pictures an SVG holds (the formula, typeset).
fn exported(path: &Path) -> String {
    if path.extension().is_some_and(|e| e == "pdf") {
        return match accent_core::pdf::PdfDoc::open(path).and_then(|d| d.page_sizes()) {
            Ok(sizes) => format!("pages={} sizes={sizes:?}", sizes.len()),
            Err(e) => format!("unreadable {e}"),
        };
    }
    let Ok(texture) = gdk::Texture::from_filename(path) else {
        return "unreadable".to_string();
    };
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, _) = downloader.download_bytes();
    let mut counts: HashMap<&[u8], usize> = HashMap::new();
    for pixel in bytes.chunks_exact(4) {
        *counts.entry(pixel).or_default() += 1;
    }
    let paper = counts.into_iter().max_by_key(|(_, n)| *n).map(|(p, _)| p);
    let pictures = match path.extension().is_some_and(|e| e == "svg") {
        true => std::fs::read_to_string(path).map_or(0, |svg| svg.matches("<image").count()),
        false => 0,
    };
    format!(
        "size={}x{} paper={paper:?} pictures={pictures}",
        texture.width(),
        texture.height()
    )
}

/// Walk the sample (written to `rel` when there is none) through Light, Dark and Solarized, each
/// also inverted (Invert Diagram Colours), painting page 1 into `<dir>/look-<theme>[-inverted].png`
/// and printing: the paper, a pixel of shape `a`'s fill and what the remap wants of `#dae8fc`, the
/// pixel of formula `m` farthest from the paper, the Properties pane's Fill for `a`, whether the
/// model and the file are as they were, and whether the tab is dirty. On a page with its page
/// view off the paper is read at the canvas's corner.
fn look(app: &Rc<App>, rel: &str, dir: &Path) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    std::fs::create_dir_all(dir).expect("a directory for the pictures");
    let (app, rel, dir) = (app.clone(), rel.to_string(), dir.to_path_buf());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        let (text, disk) = (tab.text(), std::fs::read(tab.path()).ok());
        let sheet = tab.file().pages[0].page_view();
        for theme in [Theme::Light, Theme::Dark, Theme::Solarized] {
            crate::theme::apply(theme);
            app.restyle_all();
            for inverted in [false, true] {
                glib::timeout_future(Duration::from_millis(500)).await;
                if inverted {
                    let _ = WidgetExt::activate_action(&app.window, "win.diagram-invert", None);
                }
                tab.select(vec!["a".to_string()]);
                let props_fill = tab.props_fill();
                tab.select(Vec::new());
                let started = Instant::now();
                while tab.typesetting() && started.elapsed() < Duration::from_secs(10) {
                    glib::timeout_future(Duration::from_millis(20)).await;
                }
                bench_pump();
                let canvas = tab.key_target();
                let Some(shot) = picture(&canvas) else {
                    println!("bench diagram look no_picture");
                    continue;
                };
                let name = format!(
                    "look-{theme:?}{}.png",
                    if inverted { "-inverted" } else { "" }
                );
                let _ = shot.save_to_png(dir.join(name));
                let mut downloader = gdk::TextureDownloader::new(&shot);
                downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
                let (bytes, stride) = downloader.download_bytes();
                let at = |x: f64, y: f64| {
                    let i = y as usize * stride + x as usize * 4;
                    [bytes[i], bytes[i + 1], bytes[i + 2]]
                };
                let on_page = |x: f64, y: f64| {
                    let r = tab.to_widget(&accent_drawio::Rect::new(x, y, 0.0, 0.0));
                    at(r.x, r.y)
                };
                let paper = match sheet {
                    true => on_page(790.0, 490.0),
                    false => at(3.0, 3.0),
                };
                let fill_a = on_page(124.0, 115.0);
                let dark = adw::StyleManager::default().is_dark() != inverted;
                let want = match crate::theme::page_colours(dark) {
                    Some((p, i)) => {
                        let [r, g, b, _] =
                            accent_core::recolour::recolour_pixel([0xda, 0xe8, 0xfc, 255], p, i);
                        [r, g, b]
                    }
                    None => [0xda, 0xe8, 0xfc],
                };
                let luma = |c: [u8; 3]| c.iter().map(|v| i32::from(*v)).sum::<i32>();
                let m = tab
                    .frame_of("m")
                    .map(|r| tab.to_widget(&r))
                    .unwrap_or_default();
                let mut glyph = paper;
                for y in m.y.max(0.0) as usize..(m.y + m.h) as usize {
                    for x in m.x.max(0.0) as usize..(m.x + m.w) as usize {
                        let c = at(x as f64, y as f64);
                        if (luma(c) - luma(paper)).abs() > (luma(glyph) - luma(paper)).abs() {
                            glyph = c;
                        }
                    }
                }
                let unchanged = tab.text() == text && std::fs::read(tab.path()).ok() == disk;
                let rgb = |c: [u8; 3]| format!("{},{},{}", c[0], c[1], c[2]);
                println!(
                    "bench diagram look theme={theme:?} inverted={inverted} dark={dark} \
                     paper={} fill_a={} want={} glyph={} props_fill={props_fill} \
                     text_unchanged={unchanged} dirty={}",
                    rgb(paper),
                    rgb(fill_a),
                    rgb(want),
                    rgb(glyph),
                    tab.save.modified.get()
                );
                if inverted {
                    let _ = WidgetExt::activate_action(&app.window, "win.diagram-invert", None);
                }
            }
        }
        bench_quit(&app);
    });
}

fn edit_round(app: &Rc<App>, tab: &Rc<crate::diagram::DiagramTab>) {
    let bounds = |tab: &crate::diagram::DiagramTab| tab.frame_of("a");
    tab.select(vec!["a".to_string()]);
    println!("bench diagram select a frame={:?}", bounds(tab));
    tab.nudge(30.0, 0.0);
    println!(
        "bench diagram nudge frame={:?} modified={}",
        bounds(tab),
        tab.save.modified.get()
    );
    tab.edit(|e, page| e.set_label_markdown(page, "a", "**bold** A"));
    println!("bench diagram label markdown={:?}", tab.label_markdown("a"));
    tab.undo();
    println!("bench diagram undo markdown={:?}", tab.label_markdown("a"));
    tab.undo();
    println!(
        "bench diagram undo frame={:?} history={:?}",
        bounds(tab),
        tab.history()
    );
    tab.redo();
    let etag = tab.save.etag.get();
    let flushed = app.flush_diagram(tab);
    println!(
        "bench diagram flush {:?} modified={} etag_moved={}",
        flushed.map_err(|e| e.to_string()),
        tab.save.modified.get(),
        tab.save.etag.get() != etag
    );
    let disk = std::fs::read(tab.path()).expect("the file");
    let back = accent_drawio::File::from_bytes(&disk).expect("it reads back");
    let moved = back.pages[0]
        .cell("a")
        .and_then(|c| c.geometry.as_ref())
        .map(|g| g.x);
    println!(
        "bench diagram reparse pages={} a.x={moved:?}",
        back.pages.len()
    );

    let floor = || {
        app.sidebar_column
            .measure(gtk::Orientation::Horizontal, -1)
            .0
    };
    let sidebar = app.sidebar.get().expect("a vault window has a sidebar");
    app.show_pane("properties");
    tab.select(vec!["a".to_string()]);
    tab.edit_label();
    // A picture of the window as it stands, for looking at rather than asserting on.
    if let Ok(path) = std::env::var("ACCENT_BENCH_SHOT") {
        bench_pump();
        println!(
            "bench diagram shot {}",
            shoot(app.window.upcast_ref(), Path::new(&path))
        );
    }
    tab.finish_label();
    // The label editor over a shape and over a top-aligned text cell, against the cell on
    // screen: how far it reaches past the cell's top and bottom edges, and how tall its text is
    // laid out in it.
    for id in ["a", "m"] {
        tab.select(vec![id.to_string()]);
        tab.edit_label();
        // Laid out at its font, which a provider brings in on a frame of its own.
        for _ in 0..20 {
            bench_pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        let cell = tab.frame_of(id).map(|r| tab.to_widget(&r));
        if let (Some(cell), Some((at, content, px, _))) = (cell, tab.label_at()) {
            let rect =
                |r: accent_drawio::Rect| format!("{:.1},{:.1},{:.1},{:.1}", r.x, r.y, r.w, r.h);
            println!(
                "bench diagram label {id} cell={} editor={} content={content:.1} px={px:.1} \
                 over_top={:.1} over_bottom={:.1}",
                rect(cell),
                rect(at),
                cell.y - at.y,
                at.y + at.h - (cell.y + cell.h)
            );
        }
        tab.finish_label();
    }
    println!(
        "bench diagram properties shown={} showing={} sidebar_floor={}",
        sidebar.has_pane("properties"),
        sidebar.is_showing("properties"),
        floor()
    );
    app.open_path("note.md");
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let sidebar = app.sidebar.get().expect("a sidebar");
        println!(
            "bench diagram over a note properties={} showing={:?} sidebar_floor={}",
            sidebar.has_pane("properties"),
            ["outline", "properties", "files"].map(|p| sidebar.is_showing(p)),
            app.sidebar_column
                .measure(gtk::Orientation::Horizontal, -1)
                .0
        );
        bench_quit(&app);
    });
}

/// The Properties pane over the sample (written to `rel` when there is none): shape `a`'s
/// Position and Size rows before and after a nudge, X and Width changed in one burst and taken
/// back by one undo, the Position group's buttons away and then out while the pointer is on it,
/// Copy Position read back off the clipboard and pasted onto `b`, `a`'s style pasted onto `b`,
/// and a size pasted from a clipboard holding a style, with the toast each says.
fn props(app: &Rc<App>, rel: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        // Once the session restore has put Files back.
        app.show_pane("properties");
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        let pane = tab.properties();
        let group = |title: &str| {
            find_widget(&pane, &|w| {
                w.downcast_ref::<adw::PreferencesGroup>()
                    .is_some_and(|g| g.title() == title)
            })
        };
        // In its group: the Page group has a Width and a Height of its own.
        let spin = |title: &str| {
            let within = group(if title.len() == 1 { "Position" } else { "Size" })?;
            find_widget(&within, &|w| {
                w.downcast_ref::<adw::SpinRow>()
                    .is_some_and(|r| r.title() == title)
            })
            .and_downcast::<adw::SpinRow>()
        };
        let rows = || ["X", "Y", "Width", "Height"].map(|t| spin(t).map(|r| r.text().to_string()));
        let click = |tooltip: &str| {
            let button = find_widget(&pane, &|w| {
                w.is::<gtk::Button>() && w.tooltip_text().as_deref() == Some(tooltip)
            });
            match button.and_downcast::<gtk::Button>() {
                Some(button) => button.emit_clicked(),
                None => println!("bench diagram props no_button {tooltip}"),
            }
        };
        let style = |id: &str| tab.file().pages[0].cell(id).map(|c| c.style.to_string());
        tab.select(vec!["a".to_string()]);
        println!("bench diagram props a rows={:?}", rows());
        tab.nudge(30.0, 0.0);
        println!("bench diagram props nudged rows={:?}", rows());
        if let (Some(x), Some(w)) = (spin("X"), spin("Width")) {
            x.set_value(250.0);
            w.set_value(200.0);
        }
        glib::timeout_future(Duration::from_millis(600)).await;
        println!(
            "bench diagram props burst frame={:?} rows={:?}",
            tab.frame_of("a"),
            rows()
        );
        tab.undo();
        println!("bench diagram props undo frame={:?}", tab.frame_of("a"));
        tab.redo();

        // PRELIGHT by hand, Xvfb having no pointer: the flag GTK puts on what it is over.
        let group = group("Position");
        let revealed = || {
            let group = group.as_ref()?;
            let revealer = find_widget(group, &|w| w.is::<gtk::Revealer>());
            Some(revealer.and_downcast::<gtk::Revealer>()?.reveals_child())
        };
        let away = revealed();
        if let Some(group) = &group {
            group.set_state_flags(gtk::StateFlags::PRELIGHT, false);
        }
        println!(
            "bench diagram props hover away={away:?} out={:?}",
            revealed()
        );
        // A picture of the pane with the buttons out, for looking at rather than asserting on.
        if let Ok(path) = std::env::var("ACCENT_BENCH_SHOT") {
            glib::timeout_future(Duration::from_millis(400)).await;
            println!(
                "bench diagram shot {}",
                shoot(app.window.upcast_ref(), Path::new(&path))
            );
        }
        if let Some(group) = &group {
            group.unset_state_flags(gtk::StateFlags::PRELIGHT);
        }

        click("Copy Position");
        let said = super::export::toast(&app, "Copied position").await;
        let clipboard = tab.key_target().clipboard().read_text_future().await;
        println!(
            "bench diagram props copy said={said:?} clipboard={:?}",
            clipboard.ok().flatten()
        );
        tab.select(vec!["b".to_string()]);
        click("Paste Position");
        glib::timeout_future(Duration::from_millis(300)).await;
        println!(
            "bench diagram props paste_position b={:?}",
            tab.frame_of("b")
        );

        tab.select(vec!["a".to_string()]);
        click("Copy Style");
        let said = super::export::toast(&app, "Copied style").await;
        tab.select(vec!["b".to_string()]);
        click("Paste Style");
        glib::timeout_future(Duration::from_millis(300)).await;
        println!(
            "bench diagram props paste_style said={said:?} a={:?} b={:?}",
            style("a"),
            style("b")
        );
        click("Paste Size");
        let said = super::export::toast(&app, "No size").await;
        println!(
            "bench diagram props paste_size said={said:?} b={:?} history={:?}",
            tab.frame_of("b"),
            tab.history()
        );
        bench_quit(&app);
    });
}

/// The Properties pane's Layers group over a slide template (written to `rel` when there is
/// none): the template's shape not picked on its locked layer and picked once it is unlocked,
/// let go of when it is locked again; the Content layer hidden, its shape and edge undrawn; a
/// layer added on top and current, a paste landing in it, Content picked and a paste landing
/// there; the new layer moved down, Content renamed, and the template's layer deleted with its
/// shape and the edge from it, as one step; then the lock and the hiding written to the file.
fn layers(app: &Rc<App>, rel: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, LAYERED).expect("write the layered diagram");
    }
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        app.show_pane("properties");
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        let find = |root: &gtk::Widget, tooltip: &str| {
            let found = find_widget(root, &|w| {
                w.is::<gtk::Button>() && w.tooltip_text().as_deref() == Some(tooltip)
            });
            found.and_downcast::<gtk::Button>()
        };
        let press = |id: &str, tooltip: &str| {
            let row = tab.layer_row(id);
            match row.and_then(|r| find(r.upcast_ref(), tooltip)) {
                Some(button) => button.emit_clicked(),
                None => println!("bench diagram layers no_button {id} {tooltip}"),
            }
        };
        let rows = || tab.layer_rows();
        let parent = |id: &str| {
            let file = tab.file();
            file.pages[0].cell(id).and_then(|c| c.parent.clone())
        };
        let cells = || {
            let file = tab.file();
            let ids = file.pages[0].cells.iter().map(|c| c.id.clone());
            ids.collect::<Vec<_>>()
        };
        println!(
            "bench diagram layers open rows={:?} pick_bg={:?}",
            rows(),
            tab.pick("bg")
        );
        // Every revealer in a row, its Move and Delete buttons' and libadwaita's own, before and
        // while the pointer is on it (PRELIGHT by hand, Xvfb having no pointer).
        if let Some(row) = tab.layer_row("2") {
            let revealers = || {
                let mut out = Vec::new();
                let mut todo = vec![row.clone().upcast::<gtk::Widget>()];
                while let Some(w) = todo.pop() {
                    if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                        out.push((r.has_css_class("accent-hover-actions"), r.reveals_child()));
                    }
                    let mut child = w.first_child();
                    while let Some(c) = child {
                        child = c.next_sibling();
                        todo.push(c);
                    }
                }
                out
            };
            let away = revealers();
            row.set_state_flags(gtk::StateFlags::PRELIGHT, false);
            println!(
                "bench diagram layers hover away={away:?} out={:?}",
                revealers()
            );
            row.unset_state_flags(gtk::StateFlags::PRELIGHT);
        }
        press("1", "Unlock Layer");
        println!(
            "bench diagram layers unlocked rows={:?} pick_bg={:?}",
            rows(),
            tab.pick("bg")
        );
        tab.select(vec!["bg".to_string()]);
        press("1", "Lock Layer");
        println!(
            "bench diagram layers relocked selection={:?}",
            tab.selection()
        );
        // Whether the ring's Add Rectangle can be pressed, and the tool in hand.
        let rect_tool = || {
            let button = find_widget(app.window.upcast_ref(), &|w| {
                w.is::<gtk::ToggleButton>() && w.tooltip_text().as_deref() == Some("Add Rectangle")
            });
            (button.is_some_and(|b| b.is_sensitive()), tab.tool())
        };
        let _ = WidgetExt::activate_action(&app.window, "win.diagram-rect", None);
        tab.select(vec!["c".to_string()]);
        let before = rect_tool();
        press("2", "Hide Layer");
        println!(
            "bench diagram layers hidden rows={:?} selection={:?} pick_c={:?} e={:?} \
             rect_tool={before:?}->{:?}",
            rows(),
            tab.selection(),
            tab.pick("c"),
            tab.frame_of("e"),
            rect_tool()
        );
        press("2", "Show Layer");
        println!("bench diagram layers shown rect_tool={:?}", rect_tool());

        match find(&tab.properties(), "Add Layer") {
            Some(add) => add.emit_clicked(),
            None => println!("bench diagram layers no_button Add Layer"),
        }
        let added = tab.file().pages[0].layers().last().map(|l| l.id.clone());
        let added = added.unwrap_or_default();
        tab.paste_text("Pasted");
        let pasted = tab.selection();
        println!(
            "bench diagram layers added rows={:?} paste_parent={:?}",
            rows(),
            pasted.first().and_then(|id| parent(id))
        );
        if let Some(row) = tab.layer_row("2") {
            row.emit_by_name::<()>("entry-activated", &[]);
        }
        tab.paste_text("Again");
        let pasted = tab.selection();
        println!(
            "bench diagram layers picked rows={:?} paste_parent={:?}",
            rows(),
            pasted.first().and_then(|id| parent(id))
        );
        press(&added, "Move Layer Down");
        if let Some(row) = tab.layer_row("2") {
            row.set_text("Slide");
            row.emit_by_name::<()>("apply", &[]);
        }
        println!("bench diagram layers moved_renamed rows={:?}", rows());
        press("1", "Delete Layer");
        println!(
            "bench diagram layers deleted rows={:?} cells={:?}",
            rows(),
            cells()
        );
        tab.undo();
        println!(
            "bench diagram layers undo rows={:?} cells={:?}",
            rows(),
            cells()
        );
        tab.redo();
        press("2", "Lock Layer");
        press(&added, "Hide Layer");
        let flushed = app.flush_diagram(&tab);
        let disk = std::fs::read(tab.path()).expect("the file");
        let back = accent_drawio::File::from_bytes(&disk).expect("it reads back");
        let written: Vec<String> = back.pages[0]
            .layers()
            .iter()
            .map(|l| {
                format!(
                    "{}:{:?} style={} attrs={:?}",
                    l.id,
                    l.label(),
                    l.style,
                    l.attrs
                )
            })
            .collect();
        println!(
            "bench diagram layers flush {:?} written={written:?}",
            flushed.map_err(|e| e.to_string())
        );
        // A picture of the group, for looking at rather than asserting on.
        if let Ok(path) = std::env::var("ACCENT_BENCH_SHOT") {
            tab.undo();
            glib::timeout_future(Duration::from_millis(400)).await;
            println!(
                "bench diagram shot {}",
                shoot(app.window.upcast_ref(), Path::new(&path))
            );
        }
        bench_quit(&app);
    });
}

fn hold(app: &Rc<App>, rel: &str, tool: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let (app, rel, tool) = (app.clone(), rel.to_string(), tool.to_string());
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        app.open_path(&rel);
        glib::timeout_add_local_once(Duration::from_millis(800), move || {
            let Some(tab) = app.active_diagram() else {
                println!("bench diagram no_tab");
                return bench_quit(&app);
            };
            // The main loop's stalls while the pointer works: a tick every 20 ms that reports a
            // gap of more than 150, which is a freeze to the reader.
            let (start, last) = (Instant::now(), Rc::new(Cell::new(Instant::now())));
            glib::timeout_add_local(Duration::from_millis(20), move || {
                let now = Instant::now();
                let gap = now.duration_since(last.replace(now));
                if gap > Duration::from_millis(150) {
                    println!(
                        "bench diagram stall {}ms at {:.1}s",
                        gap.as_millis(),
                        (now - start).as_secs_f64()
                    );
                }
                glib::ControlFlow::Continue
            });
            let canvas = tab.key_target();
            // Screen coordinates, for XTEST: the window sits at 0,0 with no window manager, but
            // its client-side shadow puts the widgets a margin in from the surface's corner.
            let (sx, sy) = app.window.surface_transform();
            let centre = |id: &str| {
                let r = tab.frame_of(id).map(|r| tab.to_widget(&r))?;
                canvas
                    .compute_point(
                        &app.window,
                        &graphene::Point::new((r.x + r.w / 2.0) as f32, (r.y + r.h / 2.0) as f32),
                    )
                    .map(|p| graphene::Point::new(p.x() + sx as f32, p.y() + sy as f32))
            };
            let (a, b) = (centre("a"), centre("b"));
            let (k, t) = (centre("k"), centre("t"));
            println!(
                "bench diagram turned k={:.0},{:.0} t={:.0},{:.0}",
                k.map_or(0.0, |p| p.x()),
                k.map_or(0.0, |p| p.y()),
                t.map_or(0.0, |p| p.x()),
                t.map_or(0.0, |p| p.y())
            );
            if !tool.is_empty() {
                let _ =
                    WidgetExt::activate_action(&app.window, &format!("win.diagram-{tool}"), None);
            }
            // The page's origin on the screen, for aiming at any diagram's page points.
            let o = tab.to_widget(&accent_drawio::Rect::new(0.0, 0.0, 0.0, 0.0));
            let origin = canvas
                .compute_point(&app.window, &graphene::Point::new(o.x as f32, o.y as f32))
                .map(|p| (p.x() + sx as f32, p.y() + sy as f32))
                .unwrap_or_default();
            println!(
                "bench diagram at a={:.0},{:.0} b={:.0},{:.0} scale={:.3} origin={:.1},{:.1}",
                a.map_or(0.0, |p| p.x()),
                a.map_or(0.0, |p| p.y()),
                b.map_or(0.0, |p| p.x()),
                b.map_or(0.0, |p| p.y()),
                tab.scale(),
                origin.0,
                origin.1
            );
            glib::timeout_add_local_once(Duration::from_secs(10), move || {
                let canvas = tab.key_target();
                let r = tab
                    .frame_of("a")
                    .map(|r| tab.to_widget(&r))
                    .unwrap_or_default();
                let at = canvas.compute_point(
                    &app.window,
                    &graphene::Point::new((r.x + r.w / 2.0) as f32, (r.y + r.h / 2.0) as f32),
                );
                println!(
                    "bench diagram a_now={:?} editing={:?} labels={:?}",
                    at.map(|p| (p.x(), p.y())),
                    tab.editing_label(),
                    ["a", "b", "e"].map(|id| tab.label_markdown(id))
                );
                let label = tab.label_at();
                println!(
                    "bench diagram label at={:?} px={:?} focus={:?} scale={:.3}",
                    label.map(|(at, ..)| (at.x.round(), at.y.round())),
                    label.map(|(.., px, _)| (px * 10.0).round() / 10.0),
                    label.map(|(.., focus)| focus),
                    tab.scale()
                );
                println!(
                    "bench diagram after a={:?} b={:?} selection={:?} history={:?}",
                    tab.frame_of("a"),
                    tab.frame_of("b"),
                    tab.selection(),
                    tab.history()
                );
                if let Ok(path) = std::env::var("ACCENT_BENCH_SHOT") {
                    println!(
                        "bench diagram shot {}",
                        shoot(app.window.upcast_ref(), Path::new(&path))
                    );
                }
                // What the gesture made, as the file will say it.
                let xml = tab.text();
                let file = accent_drawio::File::from_bytes(xml.as_bytes()).expect("our own XML");
                for cell in file.pages[0]
                    .cells
                    .iter()
                    .filter(|c| !["0", "1", "a", "b", "e", "m", "k", "t"].contains(&c.id.as_str()))
                {
                    println!(
                        "bench diagram new {} label={:?} style={} source={:?} target={:?} geometry={:?} ends={:?}",
                        if cell.edge { "edge" } else { "vertex" },
                        cell.label(),
                        cell.style,
                        cell.source,
                        cell.target,
                        cell.geometry
                            .as_ref()
                            .map(|g| (g.x, g.y, g.width, g.height)),
                        cell.geometry
                            .as_ref()
                            .map(|g| (g.source_point, g.target_point))
                    );
                }
                bench_quit(&app);
            });
        });
    });
}

fn preview(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        let file = tab.file();
        let Some((index, page)) = file
            .pages
            .iter()
            .enumerate()
            .max_by_key(|(_, p)| p.cells.len())
        else {
            return bench_quit(&app);
        };
        tab.show_page(index);
        bench_pump();
        let layers: Vec<&str> = page.layers().iter().map(|c| c.id.as_str()).collect();
        let top: Vec<String> = page
            .cells
            .iter()
            .filter(|c| c.parent.as_deref().is_some_and(|p| layers.contains(&p)))
            .map(|c| c.id.clone())
            .collect();
        let edges_on = |id: &str| {
            let on = |end: &Option<String>| end.as_deref() == Some(id);
            page.cells
                .iter()
                .filter(|c| on(&c.source) || on(&c.target))
                .count()
        };
        let busiest = top
            .iter()
            .filter(|id| page.cell(id).is_some_and(|c| c.vertex))
            .max_by_key(|id| edges_on(id))
            .cloned()
            .unwrap_or_default();
        // Each case's frames: the first, then the median and the worst of the rest.
        let run = |name: &str, ids: &[String], moving: bool| {
            let frames: Vec<(f64, f64)> = (1..=30)
                .map(|i| {
                    let delta = moving
                        .then(|| accent_drawio::Point::new(f64::from(i) * 7.0, f64::from(i) * 3.0));
                    tab.bench_move(ids, delta)
                })
                .collect();
            tab.bench_move(ids, None);
            let mut rest: Vec<f64> = frames[1..].iter().map(|f| f.0 + f.1).collect();
            rest.sort_by(f64::total_cmp);
            println!(
                "bench diagram preview {name} moved={} first_ms={:.2} (update {:.2}) median_ms={:.2} max_ms={:.2} median_update_ms={:.2}",
                ids.len(),
                frames[0].0 + frames[0].1,
                frames[0].0,
                rest[rest.len() / 2],
                rest[rest.len() - 1],
                {
                    let mut u: Vec<f64> = frames[1..].iter().map(|f| f.0).collect();
                    u.sort_by(f64::total_cmp);
                    u[u.len() / 2]
                }
            );
        };
        println!(
            "bench diagram preview page={index} cells={} busiest={busiest} edges={}",
            page.cells.len(),
            edges_on(&busiest)
        );
        run("still", &[], false);
        run("live", std::slice::from_ref(&busiest), true);
        run("boxed", &top, true);
        bench_quit(&app);
    });
}

/// The PDF at `pdf` with the pen in hand, presented and left, printing its ring and tool each
/// time as `bench diagram present pdf_<when>`; then the diagram at `rel` (the two-page sample when
/// there is none), zoomed in with Add Rectangle in hand, presented and left by real keys, a drag
/// and a double click over shape `a` while presented, which select and change nothing, and its
/// pages turned by key and wheel in and out of presentation. `bench diagram xtest <steps>` asks
/// for those (`build-aux/xtest.py :N "<steps>"`), and every change of what the diagram shows
/// prints as `bench diagram present <state>`.
fn present(app: &Rc<App>, rel: &str, pdf: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let (app, rel, pdf) = (app.clone(), rel.to_string(), pdf.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&pdf);
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.pdf_mode(pdfview::Mode::Pen);
        let pdf_state = |app: &Rc<App>, when: &str| {
            if let Some(pdf) = app.active_pdf() {
                println!(
                    "bench diagram present pdf_{when} presenting={} ring={} tool={:?}",
                    app.presenting.get().is_some(),
                    pdf.ring_visible(),
                    pdf.mode_label()
                );
            }
        };
        pdf_state(&app, "before");
        app.set_presenting(true);
        glib::timeout_future(Duration::from_millis(500)).await;
        pdf_state(&app, "presented");
        app.set_presenting(false);
        glib::timeout_future(Duration::from_millis(500)).await;
        pdf_state(&app, "after");

        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        for _ in 0..10 {
            tab.zoom_step(false);
        }
        let _ = WidgetExt::activate_action(&app.window, "win.diagram-rect", None);
        glib::timeout_future(Duration::from_millis(300)).await;
        // A widget point on the screen, for XTEST: the window sits at 0,0 under Xvfb, its
        // client-side shadow putting the widgets a margin in.
        let canvas = tab.key_target();
        let (sx, sy) = app.window.surface_transform();
        let on_screen = |x: f64, y: f64| {
            let p = graphene::Point::new(x as f32, y as f32);
            let p = canvas.compute_point(&app.window, &p).unwrap_or(p);
            (p.x() + sx as f32, p.y() + sy as f32)
        };
        let (mx, my) = on_screen(
            f64::from(canvas.width()) / 2.0,
            f64::from(canvas.height()) / 2.0,
        );
        println!("bench diagram xtest move {mx:.0} {my:.0}; focus; sleep 0.5; key F5");
        let (mut last, mut asked) = (String::new(), false);
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(14) {
            // Presented, the page is fitted to a larger canvas: `a` is aimed at from there.
            if app.presenting.get().is_some() && !asked {
                asked = true;
                glib::timeout_future(Duration::from_millis(300)).await;
                let a = tab
                    .frame_of("a")
                    .map(|r| tab.to_widget(&r))
                    .unwrap_or_default();
                let (ax, ay) = on_screen(a.x + a.w / 2.0, a.y + a.h / 2.0);
                println!(
                    "bench diagram xtest drag {ax:.0} {ay:.0} {:.0} {:.0}; move {ax:.0} {ay:.0}; \
                     down; up; down; up; sleep 0.5; key Delete; key ctrl+z; key Down; sleep 0.3; \
                     key space; sleep 0.3; key Left; sleep 0.3; key Right; sleep 0.3; \
                     key shift+space; sleep 0.3; key Page_Down; sleep 0.3; key Page_Up; \
                     sleep 0.3; scroll -1; sleep 0.3; scroll 1; sleep 0.3; key F5; sleep 0.5; \
                     move {mx:.0} {my:.0}; scroll -12; sleep 0.5; scroll 24",
                    ax + 150.0,
                    ay + 80.0
                );
            }
            let place = tab.place();
            let state = format!(
                "presenting={} page={} zoom={} y={:.0} ring={} tool={:?} selected={} \
                 history={:?} editing={:?} a={:?}",
                app.presenting.get().is_some(),
                place.page + 1,
                tab.zoom_label(),
                place.y,
                tab.ring_visible(),
                tab.tool(),
                tab.selection().len(),
                tab.history(),
                tab.editing_label(),
                tab.frame_of("a").map(|r| (r.x, r.y))
            );
            if state != last {
                println!("bench diagram present {state}");
                last = state;
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        bench_quit(&app);
    });
}
