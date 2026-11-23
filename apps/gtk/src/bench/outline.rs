//! The Outline pane following the caret, and the numbers its rows and thumbnails show.

use super::git::xtest;
use super::*;

/// Caret positions covering section changes and returns to the top, clamped to note length.
const WALK: [i32; 8] = [0, 6, 60, 120, 400, 0, 12, 190];

/// Follow [`WALK`] caret positions, then switch tabs and hide/show Outline to check
/// selection catch-up. `hold:<note>` prints input targets and pane changes for 20 s
/// while XTEST drives real input.
pub(super) fn bench_outline(app: &Rc<App>, arg: &str) {
    if let Some(rel) = arg.strip_prefix("index:") {
        return bench_outline_index(app, rel);
    }
    if arg == "hover" {
        return bench_outline_hover(app);
    }
    let (hold, note, other) = match arg.strip_prefix("hold:") {
        Some(note) => (true, note, ""),
        None => match arg.split_once(',') {
            Some((note, other)) => (false, note, other),
            None => return bench_quit(app),
        },
    };
    let other = other.to_string();
    app.open_path(note);
    app.show_pane("outline");
    let app = app.clone();
    // Long enough for the language layer's first answer, which is what fills the pane.
    glib::timeout_add_local_once(Duration::from_millis(1000), move || {
        let Some(tab) = app.active() else {
            return bench_quit(&app);
        };
        tab.view.grab_focus();
        if hold {
            return bench_outline_hold(&app, &tab);
        }
        bench_outline_walk(app, tab, other, 0);
    });
}

/// `index:<rel_code>` probes Rust/Python Outline fallback to the index. Print initial
/// rows/server status, then server answers 6 s later. Activate a row, follow the caret,
/// append a function and save; require refreshed symbols. Writes files; use `/tmp`.
fn bench_outline_index(app: &Rc<App>, rel: &str) {
    scratch_only(app, "ACCENT_BENCH_OUTLINE=index");
    app.open_path(rel);
    app.show_pane("outline");
    let app = app.clone();
    glib::spawn_future_local(async move {
        let rows = |app: &Rc<App>, tab: &Rc<Tab>, when: &str| {
            let lines = app.sidebar.get().map(|s| s.outline_lines(None));
            let missing = tab.lang.support().and_then(|s| s.missing.clone());
            println!(
                "bench outline index {when} served={} missing={missing:?}",
                tab.lang.served()
            );
            for line in lines.unwrap_or_default() {
                println!("bench outline index {when} row {line}");
            }
        };
        let mut tab = None;
        for _ in 0..75 {
            glib::timeout_future(Duration::from_millis(200)).await;
            tab = app.active().filter(|t| !t.lang.outline().is_empty());
            if tab.is_some() {
                break;
            }
        }
        let Some(tab) = tab else {
            println!("bench outline index no_rows");
            return bench_quit(&app);
        };
        rows(&app, &tab, "first");
        glib::timeout_future(Duration::from_secs(6)).await;
        rows(&app, &tab, "later");

        let outline = tab.lang.outline();
        let last = outline.len() - 1;
        if let Some(sidebar) = app.sidebar.get() {
            sidebar.outline_lines(Some(last));
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let caret = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
        let mut end = caret;
        end.forward_to_line_end();
        let mut start = caret;
        start.set_line_offset(0);
        println!(
            "bench outline index jump row={:?} caret={}:{} line={:?}",
            outline[last].1,
            caret.line(),
            caret.line_offset(),
            tab.buffer.text(&start, &end, false).trim()
        );

        tab.view.grab_focus();
        let body = outline
            .iter()
            .find(|(_, name, _)| !name.starts_with("impl "));
        if let Some((_, _, at)) = body {
            let line = at.line as i32 + 1;
            tab.buffer
                .place_cursor(&tab.buffer.iter_at_line(line).expect("bench line"));
            glib::timeout_future(Duration::from_millis(250)).await;
            println!(
                "bench outline index follow {}",
                outline_state(&app, &tab, line)
            );
        }

        let added = match tab.rel().ends_with(".py") {
            true => "\ndef added_by_drill():\n    pass\n",
            false => "\nfn added_by_drill() {}\n",
        };
        tab.buffer.insert(&mut tab.buffer.end_iter(), added);
        let _ = WidgetExt::activate_action(&app.window, "win.save", None);
        for _ in 0..50 {
            glib::timeout_future(Duration::from_millis(200)).await;
            if tab
                .lang
                .outline()
                .iter()
                .any(|row| row.1 == "added_by_drill")
            {
                break;
            }
        }
        rows(&app, &tab, "saved");
        bench_quit(&app);
    });
}

/// One step of the walk, and the rest of the drill once the walk is over.
fn bench_outline_walk(app: Rc<App>, tab: Rc<Tab>, other: String, step: usize) {
    let Some(&line) = WALK.get(step) else {
        return bench_outline_away(app, tab, other);
    };
    let line = line.min(tab.buffer.line_count() - 1);
    tab.buffer
        .place_cursor(&tab.buffer.iter_at_line(line).expect("bench line"));
    // Past the caret hook's 100 ms.
    glib::timeout_add_local_once(Duration::from_millis(250), move || {
        println!("bench outline walk {}", outline_state(&app, &tab, line));
        bench_outline_walk(app, tab, other, step + 1);
    });
}

/// Another tab and back, then the pane out of sight while the caret moves.
fn bench_outline_away(app: Rc<App>, tab: Rc<Tab>, other: String) {
    app.open_path(&other);
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        app.reveal_page(&tab.page);
        tab.view.grab_focus();
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
            println!("bench outline back {}", outline_state(&app, &tab, line));
            bench_outline_hidden(app, tab, 0);
        });
    });
}

/// The caret moved while the pane is out of sight, behind the Files pane and then with the whole
/// sidebar hidden: the row it had while hidden, which should not have moved, and once shown.
fn bench_outline_hidden(app: Rc<App>, tab: Rc<Tab>, step: i32) {
    match step {
        0 => app.show_pane("files"),
        1 => app.sidebar_column.set_visible(false),
        _ => return bench_outline_edit(app, tab, 0),
    }
    let line = tab.buffer.line_count() * (step + 1) / 3;
    tab.buffer
        .place_cursor(&tab.buffer.iter_at_line(line).expect("bench line"));
    glib::timeout_add_local_once(Duration::from_millis(250), move || {
        println!(
            "bench outline hidden{step} {}",
            outline_state(&app, &tab, line)
        );
        match step {
            0 => app.show_pane("outline"),
            _ => app.sidebar_column.set_visible(true),
        }
        glib::timeout_add_local_once(Duration::from_millis(250), move || {
            println!(
                "bench outline shown{step} {}",
                outline_state(&app, &tab, line)
            );
            bench_outline_hidden(app, tab, step + 1);
        });
    });
}

/// Keep Outline scroll/selection stable through a heading edit, insertion and undo
/// while the list is scrolled away from the caret. A subsequent caret move must resume
/// following the active section.
fn bench_outline_edit(app: Rc<App>, tab: Rc<Tab>, step: usize) {
    let buffer = tab.buffer.clone();
    let typed = |text: &str| {
        buffer.begin_user_action();
        buffer.insert_interactive_at_cursor(text, true);
        buffer.end_user_action();
    };
    let (name, wait) = match step {
        0 => {
            let rows = tab.lang.outline();
            let Some((_, _, at)) = rows.get(rows.len() * 2 / 3) else {
                return bench_quit(&app);
            };
            let mut end = buffer.iter_at_line(at.line as i32).expect("bench line");
            end.forward_to_line_end();
            buffer.place_cursor(&end);
            ("placed", 250)
        }
        1 => {
            let list = app
                .sidebar
                .get()
                .and_then(|s| s.outline_child())
                .and_then(|c| find_widget(&c, &|w| w.is::<gtk::ListView>()))
                .and_downcast::<gtk::ListView>()
                .expect("bench list");
            list.vadjustment().expect("bench adjustment").set_value(0.0);
            ("scrolled", 100)
        }
        2 => {
            typed(" typed");
            ("typed", 1500)
        }
        3 => {
            typed("\n## Added heading");
            ("added", 1500)
        }
        4 => {
            buffer.undo();
            buffer.undo();
            ("undone", 1500)
        }
        5 => {
            let mut next = buffer.iter_at_mark(&buffer.get_insert());
            next.forward_line();
            buffer.place_cursor(&next);
            ("moved", 250)
        }
        _ => return bench_quit(&app),
    };
    glib::timeout_add_local_once(Duration::from_millis(wait), move || {
        let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
        println!(
            "bench outline edit {name} {}",
            outline_state(&app, &tab, line)
        );
        bench_outline_edit(app, tab, step + 1);
    });
}

/// Print where to aim, then the pane's state whenever it changes.
fn bench_outline_hold(app: &Rc<App>, tab: &Rc<Tab>) {
    let aim = |widget: &gtk::Widget| {
        widget
            .compute_bounds(&app.window)
            .map(|b| format!("{:.0},{:.0}", b.x() + b.width() / 2.0, b.y() + 40.0))
    };
    let list = app.sidebar.get().and_then(|s| s.outline_child());
    println!(
        "bench outline aim editor={:?} list={:?}",
        aim(tab.view.upcast_ref()),
        list.as_ref().and_then(aim)
    );
    let (app, tab) = (app.clone(), tab.clone());
    let last = RefCell::new(String::new());
    let started = Instant::now();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
        let state = outline_state(&app, &tab, line);
        if *last.borrow() != state {
            println!("bench outline hold {state}");
            *last.borrow_mut() = state;
        }
        if started.elapsed() < Duration::from_secs(20) {
            return glib::ControlFlow::Continue;
        }
        bench_quit(&app);
        glib::ControlFlow::Break
    });
}

/// Print caret line, enclosing heading, selected row, row visibility and focus.
/// Compare with `placed` to detect selection incorrectly activating a heading.
fn outline_state(app: &Rc<App>, tab: &Rc<Tab>, placed: i32) -> String {
    let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
    let focus = match gtk::prelude::GtkWindowExt::focus(&app.window) {
        Some(w) if &w == tab.view.upcast_ref::<gtk::Widget>() => "editor".to_string(),
        Some(w) => w.type_().name().to_string(),
        None => "none".to_string(),
    };
    let list = app
        .sidebar
        .get()
        .and_then(|s| s.outline_child())
        .and_then(|c| find_widget(&c, &|w| w.is::<gtk::ListView>()))
        .and_downcast::<gtk::ListView>();
    let Some(list) = list else {
        return format!("line={line} list=none focus={focus}");
    };
    let selection = list
        .model()
        .and_downcast::<gtk::SingleSelection>()
        .expect("bench selection");
    let row = selection.selected();
    let text = selection
        .selected_item()
        .and_downcast::<gtk::StringObject>()
        .map(|s| s.string().to_string());
    // Every row is one line of the same label, so a row's place is its index times the height.
    let adj = list.vadjustment().expect("bench adjustment");
    let (value, page, upper) = (adj.value(), adj.page_size(), adj.upper());
    let height = upper / f64::from(selection.n_items().max(1));
    let top = f64::from(row) * height;
    let in_view = text.is_some() && top >= value - 0.5 && top + height <= value + page + 0.5;
    format!(
        "line={line} caret_kept={} under={:?} row={} text={text:?} in_view={in_view} \
         scroll={value:.0}/{upper:.0} page={page:.0} focus={focus}",
        line == placed,
        heading_above(tab, line),
        text.as_ref().map_or(-1, |_| row as i64),
    )
}

/// The heading a line of a note is under, read off the text: the last `#` line at or above it.
fn heading_above(tab: &Rc<Tab>, line: i32) -> Option<String> {
    let text = tab.text();
    text.lines()
        .take(line as usize + 1)
        .filter(|l| l.starts_with('#'))
        .last()
        .map(|l| l.trim_start_matches('#').trim().to_string())
}

/// A title longer than the sidebar is wide, which runs on under a row's number.
const LONG: &str = "A title that runs on well past the width of the sidebar";

/// `hover`: page and line numbers under the real pointer (XTEST), on a PDF, a diagram and a note
/// the drill writes into the vault, which must be a scratch one under `/tmp`. The second page's
/// thumbnail shows its number in its bottom-left corner. A row shows its page or line at its right
/// end, drawn over a backing of the row's own colour, its title keeping its width. With the
/// pointer gone neither shows, and on the note a line typed above the heading under the parked
/// pointer moves its number.
fn bench_outline_hover(app: &Rc<App>) {
    scratch_only(app, "ACCENT_BENCH_OUTLINE=hover");
    let root = app.root();
    let page = |(id, name): (usize, &&str)| {
        format!(
            "<diagram id=\"{id}\" name=\"{name}\"><mxGraphModel><root><mxCell id=\"0\"/>\
             <mxCell id=\"1\" parent=\"0\"/></root></mxGraphModel></diagram>"
        )
    };
    let diagram: String = ["First", LONG, "Third"]
        .iter()
        .enumerate()
        .map(page)
        .collect();
    let note = format!("# Hover\n\nSome text.\n\n## Short\n\ntext\n\n## {LONG}\n\ntext\n");
    for (rel, bytes) in [
        ("hover.pdf", outlined_pdf()),
        (
            "hover.drawio",
            format!("<mxfile>{diagram}</mxfile>").into_bytes(),
        ),
        ("hover.md", note.into_bytes()),
    ] {
        std::fs::write(root.join(rel), bytes).expect("bench fixture");
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        app.show_pane("outline");
        for (rel, short) in [
            ("hover.pdf", "Introduction"),
            ("hover.drawio", "First"),
            ("hover.md", "Short"),
        ] {
            app.open_path(rel);
            let listed = || {
                let lines = app.sidebar.get()?.outline_lines(None);
                lines.iter().any(|l| l.trim() == short).then_some(())
            };
            if until("outline hover", rel, 15_000, listed).await.is_none() {
                return bench_quit(&app);
            }
            glib::timeout_future(Duration::from_millis(800)).await;
            hover_thumbnail(&app, rel).await;
            // The row under the pointer, its number faded in over a backing of its tint, however
            // long a loaded machine takes to get there.
            let lit = |title: &str| {
                row_facts(&app, title, None).is_some_and(|f| f.hovered && f.drawn && f.matched)
            };
            for title in [short, LONG] {
                let before = row_facts(&app, title, None).map(|f| f.width);
                xtest_now(&format!(
                    "move {}",
                    row_aim(&app, title).unwrap_or_default()
                ))
                .await;
                until("outline hover", title, 5_000, || lit(title)).await;
                let after = row_facts(&app, title, Some(&format!("{rel}-{}", title.len())));
                let Some(f) = after else {
                    println!("bench outline hover {rel} row={title:?} missing");
                    continue;
                };
                println!(
                    "bench outline hover {rel} row={title:?} hovered={} number={:?} drawn={} \
                     matched={} kept={}",
                    f.hovered,
                    f.number,
                    f.drawn,
                    f.matched,
                    before == Some(f.width)
                );
            }
            if rel == "hover.md" {
                hover_parked(&app, short).await;
            }
            // Off the row from on it, a frame later: the number goes with the row's tint.
            let on = row_aim(&app, short).unwrap_or_default();
            xtest_now(&format!("move {on}")).await;
            until("outline hover", "on", 5_000, || lit(short)).await;
            xtest_now("move 5 5").await;
            glib::timeout_future(Duration::from_millis(60)).await;
            let left = row_facts(&app, short, Some(&format!("{rel}-left"))).map(|f| f.matched);
            until("outline hover", "away", 5_000, || {
                row_facts(&app, short, None).is_some_and(|f| !f.drawn)
                    && strip_number(&app).is_none_or(|s| s.number.is_none())
            })
            .await;
            let thumb = strip_number(&app).map(|s| s.number);
            let f = row_facts(&app, short, None);
            println!(
                "bench outline hover {rel} away thumb={thumb:?} row={short:?} drawn={:?} \
                 left_matched={left:?}",
                f.map(|f| f.drawn)
            );
        }
        bench_quit(&app);
    });
}

/// [`xtest`] without its settling wait, for a frame right after `steps`.
async fn xtest_now(steps: &str) {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../build-aux/xtest.py");
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let argv = ["python3", script, &display, steps].map(std::ffi::OsStr::new);
    if let Ok(run) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
        let _ = run.wait_future().await;
    }
}

/// The second page's thumbnail, scrolled to the middle of the strip, under the pointer: the
/// number it shows, and whether that sits in its bottom-left quarter.
async fn hover_thumbnail(app: &Rc<App>, rel: &str) {
    let Some(strip) = strip_number(app) else {
        return;
    };
    let (Some(&(_, my)), Some((_, top, _, height))) = (strip.middles.get(1), strip.pane) else {
        return println!("bench outline hover {rel} thumb missing");
    };
    let canvas = (app.sidebar.get().and_then(|s| s.outline_child()))
        .and_then(|c| find_widget(&c, &|w| w.has_css_class("accent-page-strip")));
    let scroller = canvas.and_then(|c| c.ancestor(gtk::ScrolledWindow::static_type()));
    if let Some(scroller) = scroller.and_downcast::<gtk::ScrolledWindow>() {
        let adjustment = scroller.vadjustment();
        adjustment.set_value(adjustment.value() + my - (top + height / 2.0));
    }
    glib::timeout_future(Duration::from_millis(300)).await;
    let Some((mx, my)) = strip_number(app).and_then(|s| s.middles.get(1).copied()) else {
        return;
    };
    let (sx, sy) = app.window.surface_transform();
    xtest(&format!("move {} {}", mx + sx, my + sy)).await;
    let shot = std::env::temp_dir().join(format!("outline-hover-{rel}-thumb.png"));
    super::minimap::window_png(&app.window, &shot);
    let Some(strip) = strip_number(app) else {
        return;
    };
    let corner = (strip.at).is_some_and(|(x, y, w, h)| x + w / 2.0 < mx && y + h / 2.0 > my);
    let number = strip.number;
    println!("bench outline hover {rel} thumb page=2 number={number:?} corner={corner}");
}

/// The pointer parked on the row `title` while a line is typed at the top of the note in front:
/// its number moves down with its heading, and its backing still matches the row. Then a heading
/// typed there, which adds a row above the others: every row on screen says its own line.
async fn hover_parked(app: &Rc<App>, title: &str) {
    xtest(&format!("move {}", row_aim(app, title).unwrap_or_default())).await;
    let Some(tab) = app.active() else {
        return;
    };
    let line = |tab: &Rc<Tab>| {
        let rows = tab.lang.outline();
        rows.iter().find(|r| r.1 == title).map(|r| r.2.line)
    };
    let was = line(&tab);
    tab.buffer
        .insert(&mut tab.buffer.start_iter(), "A line typed above.\n");
    until("outline hover", "refill", 10_000, || line(&tab) != was).await;
    glib::timeout_future(Duration::from_millis(400)).await;
    if let Some(f) = row_facts(app, title, Some("parked")) {
        println!(
            "bench outline hover parked row={title:?} hovered={} number={:?} drawn={} matched={}",
            f.hovered, f.number, f.drawn, f.matched
        );
    }
    let rows = || tab.lang.outline().len();
    let was = rows();
    tab.buffer
        .insert(&mut tab.buffer.start_iter(), "## Added\n");
    until("outline hover", "spliced", 10_000, || rows() != was).await;
    glib::timeout_future(Duration::from_millis(400)).await;
    let numbers: Vec<_> = ["Added", "Hover", title, LONG]
        .into_iter()
        .map(|t| (t, row_facts(app, t, None).and_then(|f| f.number)))
        .collect();
    println!("bench outline hover spliced numbers={numbers:?}");
}

/// What a thumbnail strip prints of its page number (`Organize::buttons`), read back.
struct Strip {
    /// Where the strip is in the window: x, y, width and height.
    pane: Option<(f64, f64, f64, f64)>,
    /// The number shown, `None` while it is away.
    number: Option<String>,
    /// Where it is in the window: x, y, width and height.
    at: Option<(f64, f64, f64, f64)>,
    /// Each thumbnail's middle in the window.
    middles: Vec<(f64, f64)>,
}

/// The document in front's thumbnail strip, if it has one.
fn strip_number(app: &Rc<App>) -> Option<Strip> {
    let window = app.window.upcast_ref::<gtk::Widget>();
    let strip = match (app.active_pdf(), app.active_diagram()) {
        (Some(pdf), _) => pdf.strip_buttons(window),
        (_, Some(d)) => d.strip_buttons(window),
        _ => return None,
    };
    let field = |name: &str| strip.split(' ').find_map(|w| w.strip_prefix(name));
    let numbers = |s: &str| -> Vec<f64> { s.split(',').filter_map(|n| n.parse().ok()).collect() };
    let some = |s: &str| {
        s.strip_prefix("Some(\"")?
            .strip_suffix("\")")
            .map(str::to_string)
    };
    let rect = |s: Option<String>| {
        let n = numbers(&s?);
        (n.len() == 4).then(|| (n[0], n[1], n[2], n[3]))
    };
    let (shown, at) = field("number=")?.split_once('@')?;
    let middles = field("thumbs=")?.split(';').map(numbers);
    let middles = middles
        .filter(|n| n.len() == 2)
        .map(|n| (n[0], n[1]))
        .collect();
    Some(Strip {
        pane: rect(some(field("strip=")?)),
        number: some(shown),
        at: rect(some(at)),
        middles,
    })
}

/// How the Outline row titled `title` is painted.
struct RowFacts {
    /// The pointer is on it.
    hovered: bool,
    /// What its number label says, shown or not.
    number: Option<String>,
    /// The number's glyphs are on screen.
    drawn: bool,
    /// Its number's backing is the colour of the row beside it.
    matched: bool,
    /// How wide its title is laid out.
    width: i32,
}

/// The row of the Outline pane's list titled `title`, as painted now, the window pictured into
/// `$TMPDIR/outline-hover-<shot>.png` when `shot` names one.
fn row_facts(app: &Rc<App>, title: &str, shot: Option<&str>) -> Option<RowFacts> {
    let row = outline_row_widget(app, title)?;
    let overlay = row.first_child().and_downcast::<gtk::Overlay>()?;
    let text = overlay.child()?.last_child().and_downcast::<gtk::Label>()?;
    let backing = overlay.last_child()?;
    let numeral = backing.first_child().and_downcast::<gtk::Label>()?;
    let window = app.window.upcast_ref::<gtk::Widget>();
    // Pictured at its own size, its border and all, which puts the window's points `origin` off
    // the picture's.
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let (w, h) = (paintable.intrinsic_width(), paintable.intrinsic_height());
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
    let origin = window.compute_bounds(window)?;
    let texture = window
        .native()?
        .renderer()?
        .render_texture(&snapshot.to_node()?, None);
    if let Some(shot) = shot {
        let path = std::env::temp_dir().join(format!("outline-hover-{shot}.png"));
        let _ = texture.save_to_png(path);
    }
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    // A widget's own points start inside its padding; its bounds take that padding in.
    let texel = |x: f32, y: f32| {
        let (x, y) = (x - origin.x(), y - origin.y());
        let i = y as usize * stride + x as usize * 4;
        bytes.get(i..i + 3).map(|p| [p[0], p[1], p[2]])
    };
    let pixel = |widget: &gtk::Widget, x: f32, y: f32| {
        let at = widget.compute_point(window, &graphene::Point::new(x, y))?;
        texel(at.x(), at.y())
    };
    let bounds = |widget: &gtk::Widget| widget.compute_bounds(window);
    let apart = |a: [u8; 3], b: [u8; 3], by: u8| a.iter().zip(b).any(|(a, b)| a.abs_diff(b) > by);
    // The row's own colour at a height of the number, in the row's padding beside it.
    let (row_at, numeral_at, backing_at) = (
        bounds(&row)?,
        bounds(numeral.upcast_ref())?,
        bounds(&backing)?,
    );
    let beside = |y: f32| texel(row_at.x() + row_at.width() - 4.0, numeral_at.y() + y);
    let backed = beside(1.0)?;
    let drawn = (0..numeral.width()).any(|x| {
        (0..numeral.height()).any(|y| {
            pixel(numeral.upcast_ref(), x as f32, y as f32).is_some_and(|p| apart(p, backed, 60))
        })
    });
    // Left of the number the backing is the row's colour through the middle half of the row, where
    // a title's letters are, clear of the row's rounded corners: the whole of it, fade and all,
    // over a title that ends before it, and the last 3 px over one running on under it.
    let ends =
        bounds(text.upcast_ref())?.x() + text.measure(gtk::Orientation::Horizontal, -1).1 as f32;
    let from = match ends < backing_at.x() {
        true => backing_at.x(),
        false => numeral_at.x() - 3.0,
    };
    let matched = (numeral.height() / 4..numeral.height() * 3 / 4).all(|y| {
        let (y, own) = (y as f32, beside(y as f32));
        (from as i32..numeral_at.x() as i32).all(|x| {
            let p = texel(x as f32, numeral_at.y() + y);
            p.zip(own).is_some_and(|(p, own)| !apart(p, own, 2))
        })
    });
    Some(RowFacts {
        hovered: row.state_flags().contains(gtk::StateFlags::PRELIGHT),
        number: backing.is_visible().then(|| numeral.text().to_string()),
        drawn,
        matched,
        width: text.width(),
    })
}

/// The list row widget of the Outline pane titled `title`, if it is on screen.
fn outline_row_widget(app: &Rc<App>, title: &str) -> Option<gtk::Widget> {
    let list = app
        .sidebar
        .get()
        .and_then(|s| s.outline_child())
        .and_then(|c| find_widget(&c, &|w| w.is::<gtk::ListView>()))?;
    let mut child = list.first_child();
    while let Some(row) = child {
        child = row.next_sibling();
        let text = (row.first_child().and_downcast::<gtk::Overlay>())
            .and_then(|o| o.child()?.last_child())
            .and_downcast::<gtk::Label>();
        if text.is_some_and(|t| t.text() == title) {
            return Some(row);
        }
    }
    None
}

/// Where XTEST puts the pointer on the row titled `title`: its middle, on screen.
fn row_aim(app: &Rc<App>, title: &str) -> Option<String> {
    let row = outline_row_widget(app, title)?;
    let middle = graphene::Point::new(row.width() as f32 / 2.0, row.height() as f32 / 2.0);
    let at = row.compute_point(&app.window, &middle)?;
    let (sx, sy) = app.window.surface_transform();
    Some(format!(
        "{} {}",
        f64::from(at.x()) + sx,
        f64::from(at.y()) + sy
    ))
}

/// Four A4 pages, their bookmarks Introduction on page 2, [`LONG`] on page 3 and Appendix on
/// page 4.
fn outlined_pdf() -> Vec<u8> {
    // 1 the catalog, 2 the page tree, 3 to 6 the pages, 7 the outline and 8 to 10 its entries.
    let entry = |title: &str, page: usize, links: &str| {
        format!(
            "<</Title({title})/Parent 7 0 R{links}/Dest[{} 0 R/Fit]>>",
            page + 2
        )
    };
    let mut objects = vec![
        "<</Type/Catalog/Pages 2 0 R/Outlines 7 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R 4 0 R 5 0 R 6 0 R]/Count 4>>".to_string(),
    ];
    objects.extend((0..4).map(|_| "<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]>>".into()));
    objects.extend([
        "<</Type/Outlines/First 8 0 R/Last 10 0 R/Count 3>>".to_string(),
        entry("Introduction", 2, "/Next 9 0 R"),
        entry(LONG, 3, "/Prev 8 0 R/Next 10 0 R"),
        entry("Appendix", 4, "/Prev 9 0 R"),
    ]);
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let (xref, size) = (out.len(), objects.len() + 1);
    out.push_str(&format!("xref\n0 {size}\n0000000000 65535 f \n"));
    for at in offsets {
        out.push_str(&format!("{at:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<</Size {size}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n"
    ));
    out.into_bytes()
}
