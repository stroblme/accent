//! The plain round: a diagram edited, undone and saved, its label editor and Properties, and
//! `shot:`'s pictures of every page.

use super::*;

pub(super) fn drill(app: &Rc<App>, shots: Option<PathBuf>) {
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
        // A window still coming up has no canvas to picture yet.
        let asked = Instant::now();
        while !tab.laid_out() && asked.elapsed() < Duration::from_secs(10) {
            glib::MainContext::default().iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        bench_pump();
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
    let flushed = app.flush_model(tab);
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
    if let Ok(path) = std::env::var("ACCENT_BENCH_DIAGRAM_SHOT") {
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
