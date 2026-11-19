//! A diagram's exports, print and note embeds, and how it looks under each theme.

use super::*;
use accent_core::config::Theme;

/// Export the sample under Dark as PDF, PNG and SVG, and print to `<dir>/print.pdf`.
/// Probe page counts/sizes, white file-paper pixels and SVG drawing, then time note
/// embeds of the first and `Two` pages. Create the sample at `rel` if absent.
pub(super) fn export(app: &Rc<App>, rel: &str, dir: &Path) {
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
            let said = super::super::export::toast(&app, &format!("Exported {name}")).await;
            println!(
                "bench diagram export {name} ms={:.0} said={said:?} {}",
                ms_since(started),
                exported(&dest)
            );
        }
        let print = dir.join("print.pdf");
        let _ = std::fs::remove_file(&print);
        let slices = crate::work::Slices::new();
        let op = operation(&tab.file(), Default::default(), "print", &slices, |_, _| {})
            .await
            .expect("the pages drawn");
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
        // A note's embed: the first page, its formulas glyphs as in an export, and page Two.
        for page in [None, Some("Two")] {
            let started = Instant::now();
            let svg = crate::diagram::embed::svg(&tab.path(), page, false, None).await;
            println!(
                "bench diagram embed page={page:?} ms={:.0} bytes={} pictures={}",
                ms_since(started),
                svg.as_ref().map_or(0, |s| s.len()),
                svg.as_ref().map_or(0, |s| s.matches("<image").count())
            );
        }
        // A click on one, which follows `rel#Two` as a link: onto that page, or a toast.
        app.open_target(&format!("{rel}#Two"));
        glib::timeout_future(Duration::from_millis(1000)).await;
        println!("bench diagram follow #Two page={}", tab.page_index());
        app.open_target(&format!("{rel}#Nope"));
        let said = super::super::export::toast(&app, "No page").await;
        println!("bench diagram follow #Nope said={said:?}");
        bench_quit(&app);
    });
}

/// A page whose drawing is 1568 units tall: a 1 wide box from (1, 1), and inside it one whose line
/// is 0 wide from (50, 1).
const TALL: &str = r#"<mxfile><diagram name="Tall" id="tall"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="a" style="fillColor=none;" vertex="1" parent="1"><mxGeometry x="1" y="1" width="100" height="1566" as="geometry"/></mxCell>
<mxCell id="z" style="fillColor=none;strokeWidth=0;" vertex="1" parent="1"><mxGeometry x="50" y="1" width="40" height="100" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"#;

/// `=render`: [`TALL`] as `accent --render-diagram` draws it, a pixel a unit to keep within 1568
/// pixels: the red of the first three pixels across the half-way row, which the 1 wide line covers
/// the middle one of, and whether the 0 wide line is drawn.
pub(super) fn render(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move { render_tall(&app).await });
}

async fn render_tall(app: &Rc<App>) {
    let file = accent_drawio::File::from_bytes(TALL.as_bytes()).expect("the tall page");
    let png = crate::diagram::export::rendered(&file, 0)
        .await
        .expect("a PNG");
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(png)).expect("decodes");
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let red = |x: usize, y: usize| bytes[y * stride + x * 4];
    let line: Vec<u8> = (0..3).map(|x| red(x, 784)).collect();
    println!(
        "bench diagram render size={}x{} line={line:?} hairline={}",
        texture.width(),
        texture.height(),
        red(50, 50) < 255
    );
    bench_quit(app);
}

/// A page of paper a shade off white holding an opaque white picture of 64 × 32 pixels in a box
/// off whole units, so pdfium draws the picture at fewer pixels than its own and at more, and any
/// seam round it shows darker than the paper.
fn edges_page() -> String {
    let white = glib::Bytes::from_owned(vec![255u8; 64 * 32 * 4]);
    let texture = gdk::MemoryTexture::new(64, 32, gdk::MemoryFormat::R8g8b8a8, &white, 64 * 4);
    let png = glib::base64_encode(&texture.save_to_png_bytes());
    format!(
        r##"<mxfile><diagram name="Edges" id="edges"><mxGraphModel page="1" pageWidth="120" pageHeight="80" background="#FAFAFA"><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="p" style="shape=image;imageAspect=0;image=data:image/png,{png};" vertex="1" parent="1"><mxGeometry x="20.3" y="15.6" width="60.37" height="30.19" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##
    )
}

/// `=edges`: [`edges_page`] as a PDF, drawn by pdfium at 72, 99, 150 and 300 dpi: whether the
/// middle of the picture is white at each, and how many pixels are darker than the paper, the
/// seam a viewer drew around a picture's box while it went in through GSK's scaled texture,
/// padded with clear black pixels under a soft mask (`paint::own_pixels` says what goes in now).
pub(super) fn edges(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let file = accent_drawio::File::from_bytes(edges_page().as_bytes()).expect("the page");
        let slices = crate::work::Slices::new();
        let pdf = crate::diagram::render::pdf(&file, Default::default(), &slices, |_, _| {});
        let path = std::env::temp_dir().join("edges.pdf");
        std::fs::write(&path, pdf.await.expect("a PDF")).expect("write the PDF");
        let doc = accent_core::pdf::PdfDoc::open(&path).expect("pdfium opens it");
        let (mut drawn, mut seams) = (true, Vec::new());
        for scale in [1.0, 1.375, 150.0 / 72.0, 300.0 / 72.0] {
            let page = doc.render_page(0, scale, accent_core::pdf::Theme::Plain);
            let page = page.expect("a rendered page");
            // The picture's middle, (50.485, 30.695) units, at 0.75 points a unit.
            let (x, y) = ((37.86 * scale) as usize, (23.02 * scale) as usize);
            let at = (y * page.width as usize + x) * 4;
            drawn &= page.data[at..at + 3] == [255, 255, 255];
            let pixels = page.data.as_chunks::<4>().0.iter();
            seams.push(pixels.filter(|p| p[..3].iter().any(|&c| c < 246)).count());
        }
        println!("bench diagram edges drawn={drawn} seams={seams:?}");
        bench_quit(&app);
    });
}

/// What an export wrote: a PDF's page sizes in points, a picture's size and its commonest pixel,
/// and the pictures and glyphs an SVG holds (a formula is glyphs, no picture).
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
    for pixel in bytes.as_chunks::<4>().0 {
        *counts.entry(pixel).or_default() += 1;
    }
    let paper = counts.into_iter().max_by_key(|(_, n)| *n).map(|(p, _)| p);
    let svg = match path.extension().is_some_and(|e| e == "svg") {
        true => std::fs::read_to_string(path).unwrap_or_default(),
        false => String::new(),
    };
    let (pictures, glyphs) = (svg.matches("<image").count(), svg.matches("<use").count());
    format!(
        "size={}x{} paper={paper:?} pictures={pictures} glyphs={glyphs}",
        texture.width(),
        texture.height()
    )
}

/// Render the sample under Light, Dark and Solarized with/without inversion as
/// `<dir>/look-<theme>[-inverted].png`. Compare paper/fill/formula pixels with the
/// remap while checking Properties, model and file colours stay unchanged and clean.
/// Without Page View, sample paper at the canvas corner.
pub(super) fn look(app: &Rc<App>, rel: &str, dir: &Path) {
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
                pumped().await;
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
                let want = match crate::theme::onto(crate::theme::recolour().diagrams, inverted) {
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
