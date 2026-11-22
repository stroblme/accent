//! Check presentation contents, available controls and layout restoration.

use super::*;

/// `ACCENT_BENCH_CHROME=present:<note>,<pdf>,<image>,<side>` presents four tab kinds
/// from the left pane. Print content bounds, hidden chrome, PDF zoom and focus;
/// `<kind>_after same=` checks layout restoration.
///
/// While presenting, probe Find (`<kind>_find`), held Ctrl+Tab card bounds/content,
/// Escape restoring the initial tab, and toast clearance above the status bar during
/// animation (`<kind>_toast*`). Repeat for a split note with an off-centre divider.
///
/// XTEST checks tab cycling, Page Down in notes/PDFs and status-bar menus: open, leave,
/// select Fit Width, open the page menu, Escape and clicks by a resting pointer.
/// `bar_menu_page_menu` must be false while the status-bar menu handles input.
/// F5/Escape in a shell must leave `presenting=false`.
/// `bench present xtest <steps>` requests `build-aux/xtest.py :N "<steps>"`, starting
/// with input focus. `bench present shot <case>` pauses 1.5 s for screenshots.
pub(super) fn bench_present(app: &Rc<App>, rels: &str) {
    let rels: Vec<String> = rels.split(',').map(str::to_string).collect();
    if rels.len() != 4 {
        println!("bench present needs <note>,<pdf>,<image>,<side>");
        return bench_quit(app);
    }
    for rel in &rels {
        app.open_path(rel);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        while !rels.iter().all(|rel| app.doc_for(rel).is_some()) {
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        let page = |rel: &str| app.doc_for(rel).map(|doc| doc.page().clone());
        let left = app.pane();
        if let Some(side) = page(&rels[3]) {
            app.split_page(&left, Side::Right, &side);
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        app.set_active_pane(&left);
        app.open_terminal();
        glib::timeout_future(Duration::from_millis(800)).await;
        let shell = app
            .docs()
            .into_iter()
            .find(|doc| doc.terminal().is_some())
            .map(|doc| doc.page().clone());
        // Off the middle, so a divider put back at its default would show.
        if let Some(split) = left.widget().parent().and_downcast::<gtk::Paned>() {
            split.set_position(split.width() * 2 / 5);
        }
        app.split.set_position(300);
        let cases = [
            ("note", page(&rels[0])),
            ("pdf", page(&rels[1])),
            ("image", page(&rels[2])),
            ("shell", shell),
        ];
        if let Some(note) = &cases[0].1 {
            app.reveal_page(note);
        }
        glib::timeout_future(Duration::from_millis(500)).await;
        app.focus_document(&left);
        glib::timeout_future(Duration::from_millis(300)).await;
        let before = layout(&app);
        println!("bench present before {before}");
        for (kind, page) in cases {
            let Some(page) = page else {
                println!("bench present {kind} missing");
                continue;
            };
            app.reveal_page(&page);
            glib::timeout_future(Duration::from_millis(500)).await;
            app.focus_document(&left);
            glib::timeout_future(Duration::from_millis(300)).await;
            let before = layout(&app);
            app.set_presenting(true);
            glib::timeout_future(Duration::from_millis(1200)).await;
            println!("bench present {kind} {}", screen(&app));
            shot(kind, kind).await;
            app.set_presenting(false);
            glib::timeout_future(Duration::from_millis(800)).await;
            let after = layout(&app);
            println!(
                "bench present {kind}_after same={} {after}",
                after == before
            );

            app.set_presenting(true);
            glib::timeout_future(Duration::from_millis(800)).await;
            app.open_find(crate::find::Mode::Find);
            glib::timeout_future(Duration::from_millis(400)).await;
            let pane = app.pane();
            let bar = pane.find.widget();
            println!(
                "bench present {kind}_find open={} in_pane={} bar={}",
                pane.find.is_open(),
                bar.is_ancestor(pane.widget()),
                drawn(&app, bar.upcast_ref())
            );
            pane.find.close();
            glib::timeout_future(Duration::from_millis(300)).await;

            app.cycle_tab(true);
            glib::timeout_future(Duration::from_millis(600)).await;
            let card = app.switcher.widget();
            let inside = card.compute_bounds(&app.window).is_some_and(|b| {
                b.x() >= 0.0
                    && b.y() >= 0.0
                    && b.x() + b.width() <= app.window.width() as f32
                    && b.y() + b.height() <= app.window.height() as f32
            });
            println!(
                "bench present {kind}_card shown={} card={} inside={inside} {}",
                app.switcher.shown(),
                drawn(&app, card),
                screen(&app)
            );
            shot(kind, &format!("{kind}_card")).await;
            // Escape as the window's key controller takes it.
            let used = app.cancel_cycle() || wire::escape_first(&app);
            glib::timeout_future(Duration::from_millis(500)).await;
            println!(
                "bench present {kind}_escape used={used} presenting={} card={} back={} {}",
                app.presenting.get().is_some(),
                app.switcher.shown(),
                pane.tabs.selected_page().as_ref() == Some(&page),
                screen(&app)
            );

            app.toasts.add(Toast::new("bench toast").lasting());
            glib::timeout_future(Duration::from_millis(600)).await;
            let (w, h) = (
                f64::from(app.window.width()),
                f64::from(app.window.height()),
            );
            app.hover_status(Some((w / 2.0, h - 4.0)));
            frames(&app, kind, "up").await;
            glib::timeout_future(Duration::from_millis(800)).await;
            println!("bench present {kind}_toast {}", toast_and_bar(&app));
            shot(kind, &format!("{kind}_toast")).await;
            app.hover_status(None);
            frames(&app, kind, "down").await;
            glib::timeout_future(Duration::from_millis(800)).await;
            println!("bench present {kind}_toast_away {}", toast_and_bar(&app));
            app.toasts.dismiss_all();
            glib::timeout_future(Duration::from_millis(500)).await;
            app.set_presenting(false);
            glib::timeout_future(Duration::from_millis(800)).await;
        }

        // The note's preview tab in a pane beside it: presenting the note hides that pane, and
        // leaving brings both back with their divider where it was. Two panes only, so the
        // divider has room to be off the middle in a window this size.
        if let Some(side) = page(&rels[3]) {
            app.close_page(&side);
        }
        if let Some(note) = page(&rels[0]) {
            app.reveal_page(&note);
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        super::preview::beside(&app).await;
        glib::timeout_future(Duration::from_millis(800)).await;
        if let Some(split) = left.widget().parent().and_downcast::<gtk::Paned>() {
            split.set_position(split.width() * 3 / 5);
        }
        app.focus_document(&left);
        glib::timeout_future(Duration::from_millis(500)).await;
        let before = layout(&app);
        println!("bench present split_before {before}");
        shot("note", "split_before").await;
        app.set_presenting(true);
        glib::timeout_future(Duration::from_millis(1200)).await;
        println!("bench present split {}", screen(&app));
        app.set_presenting(false);
        glib::timeout_future(Duration::from_millis(800)).await;
        let after = layout(&app);
        println!("bench present split_after same={} {after}", after == before);
        shot("note", "split_after").await;

        // The chord by real keys over the presented note: Escape while it is held, then a release
        // on the tab it stepped to.
        app.set_presenting(true);
        println!("bench present xtest move 500 400; focus");
        until("present", "focus", 10_000, || app.window.is_active()).await;
        glib::timeout_future(Duration::from_millis(500)).await;
        println!(
            "bench present xtest keydown ctrl; key Tab; key Tab; sleep 0.6; key Escape; keyup ctrl; \
             sleep 0.5; keydown ctrl; key Tab; sleep 0.6; keyup ctrl"
        );
        let mut last = String::new();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(6) {
            let state = format!(
                "held={} card={} presenting={} {}",
                app.cycle.borrow().is_some(),
                drawn(&app, app.switcher.widget()),
                app.presenting.get().is_some(),
                screen(&app)
            );
            if state != last {
                println!("bench present keys {state}");
                last = state;
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        }

        // Page Down read by what is presented, brought to the front as a click on the card would.
        for (kind, rel) in [("note", &rels[0]), ("pdf", &rels[1])] {
            if let Some(page) = page(rel) {
                app.reveal_page(&page);
            }
            glib::timeout_future(Duration::from_millis(1200)).await;
            let before = position(&app).await;
            println!("bench present xtest key Page_Down");
            glib::timeout_future(Duration::from_millis(1200)).await;
            let after = position(&app).await;
            println!(
                "bench present {kind}_page_down before={before} after={after} moved={} {}",
                before != after,
                screen(&app)
            );
        }
        // The status bar's menus over the presented PDF, by real pointer: the zoom readout
        // right-clicked, the pointer then up over the page and Fit Width picked; the page count
        // clicked and its menu put away by Escape with the pointer still on the bar. The pointer
        // comes onto a control only once the bar is up: GTK aims a press at the widget the last
        // motion was over, which for a pointer at rest as the bar came up is the page under it.
        let (dx, dy) = app.window.surface_transform();
        let strip = f64::from(app.window.height()) - 4.0 + dy;
        println!(
            "bench present xtest move {} {strip}",
            f64::from(app.window.width()) / 2.0 + dx
        );
        glib::timeout_future(Duration::from_millis(800)).await;
        let centre = |widget: &gtk::Widget| {
            let b = widget.compute_bounds(&app.window)?;
            let x = f64::from(b.x() + b.width() / 2.0) + dx;
            Some((x, f64::from(b.y() + b.height() / 2.0) + dy))
        };
        let zoom = app.statusbar.zoom().clone();
        let page = app
            .statusbar
            .facts_control()
            .clone()
            .upcast::<gtk::Widget>();
        let (Some((zx, zy)), Some((px, py))) = (centre(&zoom), centre(&page)) else {
            println!("bench present bar_menu bar not drawn");
            return bench_quit(&app);
        };
        let state = |case: &str| {
            println!(
                "bench present bar_menu_{case} menu={} bar={} pdf_zoom={:?}",
                app.statusbar.menu_open(),
                app.toolbar.reveals_bottom_bars(),
                app.active_pdf().and_then(|pdf| pdf.zoom_label())
            )
        };
        println!("bench present xtest move {zx} {zy}; sleep 0.3; down 3; up 3");
        glib::timeout_future(Duration::from_millis(800)).await;
        state("open");
        println!(
            "bench present xtest move {zx} {}; move {zx} {}",
            zy - 60.0,
            zy - 150.0
        );
        glib::timeout_future(Duration::from_millis(800)).await;
        state("away");
        match menu_item(&zoom, "Fit Width") {
            Some((x, y)) => {
                println!("bench present xtest move {x} {y}; sleep 0.3; down; up");
                glib::timeout_future(Duration::from_millis(800)).await;
            }
            None => println!("bench present bar_menu Fit Width not found"),
        }
        state("pick");
        println!(
            "bench present xtest move {px} {strip}; sleep 0.6; move {px} {py}; sleep 0.3; down; up"
        );
        glib::timeout_future(Duration::from_millis(1600)).await;
        state("page");
        println!("bench present xtest key Escape");
        glib::timeout_future(Duration::from_millis(800)).await;
        state("escape");
        // The pointer comes to rest on the zoom readout in one motion from above the strip, so
        // the bar comes up under a pointer whose last motion was over the page, and is pressed
        // with no motion since: right-clicked, the readout's menu and not the page's (`rest`);
        // then, the menu put away and the same again, clicked, Fit Height (`rest_click`).
        let rest = |button: u32| {
            println!(
                "bench present xtest move {zx} {}; sleep 0.8; move {zx} {zy}; sleep 0.8; \
                 down {button}; up {button}",
                zy - 150.0
            )
        };
        let page_menu = || {
            let menu = find_widget(app.window.upcast_ref(), &|w| {
                w.is::<gtk::Popover>() && w.is_visible() && !w.is_ancestor(app.statusbar.widget())
            });
            println!("bench present bar_menu_page_menu {}", menu.is_some());
        };
        rest(3);
        glib::timeout_future(Duration::from_millis(2400)).await;
        state("rest");
        page_menu();
        println!("bench present xtest key Escape");
        glib::timeout_future(Duration::from_millis(800)).await;
        rest(1);
        glib::timeout_future(Duration::from_millis(2400)).await;
        state("rest_click");
        page_menu();
        // F5 and Escape leave from a presented shell, which has every other key.
        let shell = app
            .docs()
            .into_iter()
            .find(|doc| doc.terminal().is_some())
            .map(|doc| doc.page().clone());
        for key in ["F5", "Escape"] {
            app.set_presenting(true);
            if let Some(shell) = &shell {
                app.reveal_page(shell);
            }
            glib::timeout_future(Duration::from_millis(800)).await;
            let focus = gtk::prelude::GtkWindowExt::focus(&app.window).map(|f| f.type_().name());
            println!("bench present xtest key {key}");
            glib::timeout_future(Duration::from_millis(800)).await;
            println!(
                "bench present shell_{key} focus={focus:?} presenting={}",
                app.presenting.get().is_some()
            );
        }
        bench_quit(&app);
    });
}

/// The diagram [`bench_laser`] writes: a page that is one link to a second, so a press reaching
/// it turns the page.
const LINKED: &str = r#"<mxfile host="accent">
  <diagram name="One" id="laser-1"><mxGraphModel page="0"><root><mxCell id="0"/><mxCell id="1" parent="0"/>
    <UserObject label="Two" link="data:page/id,laser-2" id="go"><mxCell style="rounded=0;" parent="1" vertex="1"><mxGeometry x="0" y="0" width="400" height="300" as="geometry"/></mxCell></UserObject>
  </root></mxGraphModel></diagram>
  <diagram name="Two" id="laser-2"><mxGraphModel page="0"><root><mxCell id="0"/><mxCell id="1" parent="0"/>
    <mxCell id="back" value="One" style="rounded=0;" parent="1" vertex="1"><mxGeometry x="0" y="0" width="400" height="300" as="geometry"/></mxCell>
  </root></mxGraphModel></diagram>
</mxfile>
"#;

/// `ACCENT_BENCH_CHROME=laser` writes `laser.drawio` ([`LINKED`]), a blank `laser.pdf`, a note
/// and `laser.py` into a scratch vault, then presents the diagram, the PDF and the note in turn
/// and puts the laser pointer up over each with a real `L` (DESIGN.md, Views). A drag across the middle prints the strokes while the
/// button is down (`<kind> drawing`), just after it is let go (`held`), into the fade (`fading`),
/// as a press there brings them back (`again`) and once they are gone (`gone`), then whether what
/// is presented stayed as it was (`kept`) and that `L` takes the pointer away (`off`); the
/// diagram's link then turns its page to a plain click (`clicked`). Under F5 an `l` typed into
/// the find bar is the query's and one typed into the code tab is its text, the palette's Laser
/// Pointer reaching the pointer there, the status bar's toggle taking it away and putting it up
/// again with real clicks (`button`), and F5 leaves with it away and the toggle gone. With
/// `ACCENT_BENCH_SHOTS` set, the window at each moment goes to `laser-<kind>-<moment>.png` there.
pub(super) fn bench_laser(app: &Rc<App>) {
    scratch_only(app, "ACCENT_BENCH_CHROME=laser");
    let pdf = accent_core::pdf::blank_pdf((595.0, 842.0)).unwrap_or_default();
    let rels = ["laser.drawio", "laser.pdf", "laser.md", "laser.py"];
    let texts: [&[u8]; 4] = [
        LINKED.as_bytes(),
        &pdf,
        b"# Laser\n\nA line to point at.\n",
        b"x = 1\n",
    ];
    for (rel, text) in rels.iter().zip(texts) {
        std::fs::write(app.root().join(rel), text).expect("bench fixture");
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        // A file written a moment ago is only in the vault once the watcher has seen it.
        glib::timeout_future(Duration::from_millis(1500)).await;
        for rel in &rels {
            app.open_path(rel);
        }
        let all = || rels.iter().all(|rel| app.doc_for(rel).is_some());
        until("laser", "opened", 20_000, all).await;
        super::git::xtest("move 600 400; focus").await;
        until("laser", "focus", 10_000, || app.window.is_active()).await;
        for (kind, rel) in ["diagram", "pdf", "note"].into_iter().zip(&rels) {
            present(&app, rel).await;
            stroke(&app, kind).await;
            // The rendered note is where the find bar is tried.
            if kind != "note" {
                app.set_presenting(false);
            }
        }
        super::git::xtest("key ctrl+f").await;
        super::git::xtest("type l").await;
        println!(
            "bench laser find query={:?} on={}",
            app.pane().find.query_text(),
            laser_on(&app)
        );
        super::git::xtest("key Escape; sleep 0.3; key l").await;
        println!("bench laser find closed on={}", laser_on(&app));
        super::git::xtest("key F5").await;
        println!(
            "bench laser left presenting={} on={}",
            app.presenting.get().is_some(),
            laser_on(&app)
        );
        present(&app, rels[3]).await;
        super::git::xtest("key End; key l").await;
        let text = app.active().map(|tab| tab.text());
        println!("bench laser code text={text:?} on={}", laser_on(&app));
        let _ = WidgetExt::activate_action(&app.window, "win.laser", None);
        println!("bench laser code palette on={}", laser_on(&app));
        press_toggle(&app).await;
        super::git::xtest("key F5").await;
        println!("bench laser code left on={}", laser_on(&app));
        toggle_said(&app, "left");
        bench_quit(&app);
    });
}

/// The keys that turn a presented page, by XTEST's names, each next-page one before the
/// previous-page one that mirrors it (DESIGN.md, Views).
const PAGING: [&str; 10] = [
    "Right",
    "Left",
    "space",
    "shift+space",
    "Page_Down",
    "Page_Up",
    "n",
    "p",
    "End",
    "Home",
];

/// Three pages, each a shape of its own.
const PAGES: &str = r#"<mxfile host="accent">
  <diagram name="One" id="paging-1"><mxGraphModel page="0"><root><mxCell id="0"/><mxCell id="1" parent="0"/><mxCell id="a" value="One" style="rounded=0;" parent="1" vertex="1"><mxGeometry x="0" y="0" width="400" height="300" as="geometry"/></mxCell></root></mxGraphModel></diagram>
  <diagram name="Two" id="paging-2"><mxGraphModel page="0"><root><mxCell id="0"/><mxCell id="1" parent="0"/><mxCell id="b" value="Two" style="rounded=0;" parent="1" vertex="1"><mxGeometry x="0" y="0" width="400" height="300" as="geometry"/></mxCell></root></mxGraphModel></diagram>
  <diagram name="Three" id="paging-3"><mxGraphModel page="0"><root><mxCell id="0"/><mxCell id="1" parent="0"/><mxCell id="c" value="Three" style="rounded=0;" parent="1" vertex="1"><mxGeometry x="0" y="0" width="400" height="300" as="geometry"/></mxCell></root></mxGraphModel></diagram>
</mxfile>
"#;

/// `ACCENT_BENCH_CHROME=paging` writes `paging.drawio` ([`PAGES`]), a blank `paging.pdf` made three
/// pages long and `wide.pdf`, eight pages much wider than tall, into a scratch vault, presents each
/// and from a page in the middle presses each of [`PAGING`] by XTEST (a PDF's but Page Down and
/// Page Up), printing the page shown
/// before and after it (`bench paging <kind> <key> 2>3`). A wide page fitted to the view is
/// shorter than half of it, so the middle of the view lies on the next page, where a turn has to
/// go on from the page at the top, and which the status bar names while presenting (`status=`).
/// Last, `End` and `Left` on the wide deck (`wide end`). The wide deck is read at Fit Height before
/// it is presented, turned once with `Right` (`wide reading`), and its scroll range and status bar
/// printed then, presented, and after it is left, at its third page and at its last (`wide range`).
/// Presented, its page is alone in the view and centred in it (`wide alone`), and stays so through
/// a wheel's notch either way and through Down, Up, Page Down and Page Up, each a turn.
pub(super) fn bench_paging(app: &Rc<App>) {
    scratch_only(app, "ACCENT_BENCH_CHROME=paging");
    let root = app.root();
    let blank = |size| accent_core::pdf::blank_pdf(size).unwrap_or_default();
    std::fs::write(root.join("paging.drawio"), PAGES).expect("bench fixture");
    std::fs::write(root.join("paging.pdf"), blank((595.0, 842.0))).expect("bench fixture");
    std::fs::write(root.join("wide.pdf"), blank((842.0, 260.0))).expect("bench fixture");
    let app = app.clone();
    glib::spawn_future_local(async move {
        // A file written a moment ago is only in the vault once the watcher has seen it.
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path("paging.drawio");
        for (rel, length) in [("paging.pdf", 3), ("wide.pdf", 8)] {
            app.open_path(rel);
            let pages = || app.active_pdf().map(|pdf| pdf.page_count());
            until("paging", rel, 20_000, || pages().filter(|n| *n > 0)).await;
            for _ in 1..length {
                if let Some(pdf) = app.active_pdf() {
                    pdf.add_page(true);
                }
            }
            until("paging", "pages", 10_000, || {
                pages().filter(|n| *n == length)
            })
            .await;
        }
        super::git::xtest("move 600 400; focus").await;
        until("paging", "focus", 10_000, || app.window.is_active()).await;
        let cases = [
            ("diagram", "paging.drawio", 1),
            ("pdf", "paging.pdf", 1),
            ("wide", "wide.pdf", 2),
        ];
        for (kind, rel, middle) in cases {
            if kind == "wide" {
                if let Some(doc) = app.doc_for(rel) {
                    app.reveal_page(doc.page());
                }
                glib::timeout_future(Duration::from_millis(500)).await;
                if let Some(pdf) = app.active_pdf() {
                    pdf.set_zoom(pdfview::PdfZoom::FitPage);
                    pdf.goto_page(middle);
                }
                glib::timeout_future(Duration::from_millis(400)).await;
                range(&app, "reading");
                // A turn goes on from the page at the top while reading too.
                app.focus_document(&app.pane());
                let before = presented_page(&app);
                super::git::xtest("key Right").await;
                println!(
                    "bench paging wide reading Right {before}>{} status={:?}",
                    presented_page(&app),
                    status(&app)
                );
                if let Some(pdf) = app.active_pdf() {
                    pdf.goto_page(middle);
                }
                glib::timeout_future(Duration::from_millis(400)).await;
            }
            present(&app, rel).await;
            if kind == "wide" {
                range(&app, "presented");
                alone(&app, "presented");
            }
            for key in PAGING {
                // A PDF's Page Down and Page Up scroll most of a screen, as GTK's do, and are no
                // turn of its pages.
                if kind != "diagram" && key.starts_with("Page_") {
                    continue;
                }
                if let Some(tab) = app.active_diagram() {
                    tab.show_page(middle);
                }
                if let Some(pdf) = app.active_pdf() {
                    pdf.goto_page(middle);
                }
                glib::timeout_future(Duration::from_millis(400)).await;
                let before = presented_page(&app);
                super::git::xtest(&format!("key {key}")).await;
                let status = match kind {
                    "wide" => format!(" status={:?}", status(&app)),
                    _ => String::new(),
                };
                println!(
                    "bench paging {kind} {key} {before}>{}{status}",
                    presented_page(&app)
                );
            }
            if kind == "wide" {
                super::git::xtest("key End").await;
                let end = (presented_page(&app), status(&app));
                super::git::xtest("key Left").await;
                println!(
                    "bench paging wide end {} {:?} Left {} {:?}",
                    end.0,
                    end.1,
                    presented_page(&app),
                    status(&app)
                );
                let turns = [
                    ("notch_down", "scroll -1"),
                    ("notch_up", "scroll 1"),
                    ("Down", "key Down"),
                    ("Up", "key Up"),
                    ("Page_Down", "key Page_Down"),
                    ("Page_Up", "key Page_Up"),
                ];
                for (name, step) in turns {
                    if let Some(pdf) = app.active_pdf() {
                        pdf.goto_page(middle);
                    }
                    glib::timeout_future(Duration::from_millis(400)).await;
                    let before = presented_page(&app);
                    super::git::xtest(&format!("{step}; sleep 0.4")).await;
                    println!(
                        "bench paging wide {name} {before}>{} status={:?}",
                        presented_page(&app),
                        status(&app)
                    );
                    alone(&app, name);
                }
                if let Some(pdf) = app.active_pdf() {
                    pdf.goto_page(middle);
                }
                glib::timeout_future(Duration::from_millis(400)).await;
            }
            app.set_presenting(false);
            glib::timeout_future(Duration::from_millis(500)).await;
            if kind == "wide" {
                range(&app, "left");
                present(&app, rel).await;
                super::git::xtest("key End").await;
                app.set_presenting(false);
                glib::timeout_future(Duration::from_millis(500)).await;
                range(&app, "left_end");
            }
        }
        bench_quit(&app);
    });
}

/// The page shown, counted from one: the presented diagram's, or the PDF page whose top is at the
/// top of the view, where a turn puts it.
fn presented_page(app: &App) -> usize {
    match (app.active_diagram(), app.active_pdf()) {
        (Some(tab), _) => tab.page_index() + 1,
        (_, Some(pdf)) => pdf.top_page() + 1,
        _ => 0,
    }
}

/// The status bar's count, where a PDF names the page being read.
fn status(app: &App) -> String {
    let label = app
        .statusbar
        .facts_control()
        .child()
        .and_downcast::<gtk::Label>();
    label
        .map(|label| label.label().to_string())
        .unwrap_or_default()
}

/// `bench paging wide range <case> top=<page> room=<bool> status=<count>`: the page at the top,
/// whether the view scrolls on past the pages' own height, which presenting once left as room
/// below the last page, and the page the status bar names. The pixels follow on a `range_px` line.
fn range(app: &App, case: &str) {
    let Some(pdf) = app.active_pdf() else {
        return println!("bench paging wide range {case} no pdf");
    };
    let (upper, content) = pdf.scroll_range();
    println!(
        "bench paging wide range {case} top={} room={} status={:?}",
        presented_page(app),
        upper > content + 0.5,
        status(app)
    );
    println!("bench paging wide range_px {case} upper={upper:.1} content={content:.1}");
}

/// `bench paging wide alone <case> page=<n> centred=<bool> beside=<pages>`: the page shown,
/// whether it sits in the middle of the view across and down, to the pixel `widget_rect` rounds
/// by, and how many other pages show any of themselves in the view.
fn alone(app: &App, case: &str) {
    let view = app.active_pdf().and_then(|pdf| {
        let view = pdf.views()[0].clone().downcast::<pdfview::PdfView>().ok()?;
        Some((pdf, view))
    });
    let Some((pdf, view)) = view else {
        return println!("bench paging wide alone {case} no pdf");
    };
    let (vw, vh) = (view.width(), view.height());
    let shown = |n: usize| {
        let (w, h) = pdf.page_size(n)?;
        view.widget_rect(n, &accent_core::pdf::Rect::from_corners((0.0, 0.0), (w, h)))
    };
    let page = pdf.top_page();
    let centred = shown(page).is_some_and(|r| {
        let (right, bottom) = (vw - r.x() - r.width(), vh - r.y() - r.height());
        (r.x() - right).abs() <= 2 && (r.y() - bottom).abs() <= 2
    });
    let beside = (0..pdf.page_count())
        .filter(|&n| n != page)
        .filter_map(shown)
        .filter(|r| r.x() < vw && r.x() + r.width() > 0 && r.y() < vh && r.y() + r.height() > 0)
        .count();
    println!(
        "bench paging wide alone {case} page={} centred={centred} beside={beside}",
        page + 1
    );
}

/// Bring `rel`'s tab to the front with the keyboard in it, and present it.
async fn present(app: &Rc<App>, rel: &str) {
    if let Some(doc) = app.doc_for(rel) {
        app.reveal_page(doc.page());
    }
    glib::timeout_future(Duration::from_millis(500)).await;
    app.focus_document(&app.pane());
    glib::timeout_future(Duration::from_millis(300)).await;
    app.set_presenting(true);
    glib::timeout_future(Duration::from_millis(1200)).await;
}

/// [`bench_laser`]'s strokes over the presented `kind`, by XTEST from `L` on.
async fn stroke(app: &Rc<App>, kind: &str) {
    super::git::xtest("key l").await;
    println!("bench laser {kind} on={}", laser_on(app));
    let before = presented(app);
    let Some(area) = app.pane().area().compute_bounds(&app.window) else {
        println!("bench laser {kind} no area");
        return;
    };
    let (dx, dy) = app.window.surface_transform();
    let (x, y) = (
        f64::from(area.center().x()) + dx,
        f64::from(area.center().y()) + dy,
    );
    let plain = red(app).await;
    // A pause between the moves, or GTK would make one motion of them.
    let path = format!(
        "move {} {y}; down; sleep 0.05; move {} {}; sleep 0.05; move {x} {y}; sleep 0.05; \
         move {} {}; sleep 0.05; move {} {y}",
        x - 150.0,
        x - 75.0,
        y - 40.0,
        x + 75.0,
        y + 40.0,
        x + 150.0
    );
    super::git::xtest_now(&path).await;
    glib::timeout_future(Duration::from_millis(100)).await;
    moment(app, kind, "drawing", Some(plain)).await;
    // The release, the 510 ms fade that begins 500 ms after it, and a press into the fade, the
    // moments between read off the laser as it hears them. The fade is held where it was seen
    // to begin, so the press lands in it however long the press takes to come under load.
    super::git::xtest_now("up").await;
    let pressed = |down: bool| move || app.laser.get().is_some_and(|l| l.pressed() == down);
    until("laser", "release", 5_000, pressed(false)).await;
    let released = Instant::now();
    glib::timeout_future(Duration::from_millis(100)).await;
    moment(app, kind, "held", Some(plain)).await;
    let fading = || app.laser.get().is_some_and(|laser| laser.strength() < 1.0);
    until("laser", "fade", 3_000, fading).await;
    let held = released.elapsed().as_millis();
    if let Some(laser) = app.laser.get() {
        laser.hold_fade();
    }
    println!("bench laser detail {kind} held_ms={held}");
    moment(app, kind, "fading", None).await;
    super::git::xtest_now("down").await;
    until("laser", "press", 10_000, pressed(true)).await;
    moment(app, kind, "again", Some(plain)).await;
    super::git::xtest_now("up").await;
    let gone = || {
        app.laser
            .get()
            .is_some_and(|laser| laser.strokes().is_empty())
    };
    until("laser", "gone", 10_000, gone).await;
    moment(app, kind, "gone", Some(plain)).await;
    let after = presented(app);
    println!("bench laser {kind} kept={} {after}", after == before);
    super::git::xtest("key l").await;
    println!("bench laser {kind} off on={}", laser_on(app));
    if kind == "diagram" {
        super::git::xtest(&format!("move {x} {y}; down; up")).await;
        println!("bench laser {kind} clicked {}", presented(app));
    }
}

fn laser_on(app: &App) -> bool {
    app.laser.get().is_some_and(|laser| laser.is_on())
}

/// The status bar's laser toggle pressed twice with real clicks, once the bar is up under the
/// pointer: the laser goes away, then comes up again, as the toggle shows (`bench laser button
/// …`). With `ACCENT_BENCH_SHOTS` set, the window goes to `laser-button.png` there after.
async fn press_toggle(app: &Rc<App>) {
    toggle_said(app, "before");
    let (dx, dy) = app.window.surface_transform();
    let strip = f64::from(app.window.height()) - 4.0 + dy;
    let x = f64::from(app.window.width()) / 2.0 + dx;
    super::git::xtest(&format!("move {x} {strip}")).await;
    // Up under the pointer and done sliding in, so the toggle is where it stays.
    let bar = app.statusbar.widget();
    let (_, full, _, _) = bar.measure(gtk::Orientation::Vertical, app.toolbar.width());
    let up = || app.toolbar.bottom_bar_height() >= full;
    if until("laser", "bar up", 5_000, up).await.is_none() {
        return;
    }
    let Some(at) = app.statusbar.laser().compute_bounds(&app.window) else {
        return println!("bench laser button not drawn");
    };
    let (x, y) = (
        f64::from(at.center().x()) + dx,
        f64::from(at.center().y()) + dy,
    );
    for (case, on) in [("pressed", false), ("again", true)] {
        super::git::xtest(&format!("move {x} {y}; sleep 0.3; down; up")).await;
        let toggle = app.statusbar.laser();
        let heard = || laser_on(app) == on && toggle.is_active() == on;
        until("laser", case, 5_000, heard).await;
        toggle_said(app, case);
    }
    if let Ok(dir) = std::env::var("ACCENT_BENCH_SHOTS") {
        super::minimap::window_png(&app.window, &Path::new(&dir).join("laser-button.png"));
    }
}

/// `bench laser button <case>`: whether the toggle is shown, the laser up and the toggle on.
fn toggle_said(app: &App, case: &str) {
    let toggle = app.statusbar.laser();
    println!(
        "bench laser button {case} shown={} on={} active={}",
        toggle.is_visible(),
        laser_on(app),
        toggle.is_active()
    );
}

/// The strokes up and how strong they are drawn at `moment`, and with the window's red before the
/// drag in `plain`, whether they are on screen; the exact numbers go on a line of their own, which
/// timing moves. A shot of the window goes to `$ACCENT_BENCH_SHOTS`.
async fn moment(app: &App, kind: &str, moment: &str, plain: Option<usize>) {
    let Some(laser) = app.laser.get() else {
        return println!("bench laser {kind} {moment} none");
    };
    let (strokes, strength) = (laser.strokes(), laser.strength());
    let shown = match strength {
        _ if strokes.is_empty() => "none",
        s if s >= 1.0 => "full",
        _ => "fading",
    };
    let now = red(app).await;
    let marked = plain
        .map(|plain| format!(" marked={}", now > plain + 1000))
        .unwrap_or_default();
    println!(
        "bench laser {kind} {moment} strokes={} strength={shown}{marked}",
        strokes.len()
    );
    println!(
        "bench laser detail {kind} {moment} points={strokes:?} strength={strength:.2} red={now}"
    );
    if let Ok(dir) = std::env::var("ACCENT_BENCH_SHOTS") {
        let shot = Path::new(&dir).join(format!("laser-{kind}-{moment}.png"));
        super::minimap::window_png(&app.window, &shot);
    }
}

/// What a press reaching the presented document would change: a diagram's page and selection, a
/// PDF's page, the rendered note's scroll and selection.
fn presented(app: &App) -> String {
    if let Some(tab) = app.active_diagram() {
        return format!(
            "page={} selected={:?}",
            tab.page_index() + 1,
            tab.selection()
        );
    }
    if let Some(pdf) = app.active_pdf() {
        return format!("page={:?}", pdf.page_label());
    }
    match app.preview.borrow().as_ref() {
        Some(preview) => format!(
            "scrollY={} selected={:?}",
            preview.scroll_y(),
            (preview.focus_target().downcast_ref::<gtk::TextView>())
                .map(|view| view.buffer().has_selection())
        ),
        None => "none".into(),
    }
}

/// How many of the window's pixels are the laser's red on screen now, in its last painted frame,
/// which is what a `GtkWidgetPaintable` of it draws. A redraw queued since, as a press or the
/// laser put up queues one, leaves it no picture until the next frame is painted, so this waits
/// for that frame rather than count nothing as no red.
async fn red(app: &App) -> usize {
    let window = app.window.upcast_ref::<gtk::Widget>();
    let painted = || {
        let (w, h) = (f64::from(window.width()), f64::from(window.height()));
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(window)).snapshot(&snapshot, w, h);
        snapshot.to_node()
    };
    let node = until("laser", "painted", 5_000, painted).await;
    let (Some(node), Some(renderer)) = (node, app.window.renderer()) else {
        return 0;
    };
    let mut downloader = gdk::TextureDownloader::new(&renderer.render_texture(&node, None));
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, _) = downloader.download_bytes();
    let pixels = bytes.as_chunks::<4>().0;
    pixels
        .iter()
        .filter(|[r, g, b, _]| {
            let (r, g, b) = (i32::from(*r), i32::from(*g), i32::from(*b));
            r - g > 90 && r - b > 90
        })
        .count()
}

/// How far the presented note is scrolled, or which page of the presented PDF is in view.
async fn position(app: &Rc<App>) -> String {
    if let Some(pdf) = app.active_pdf() {
        return pdf.page_label().unwrap_or_default();
    }
    match app.preview.borrow().as_ref() {
        Some(preview) => format!("scrollY={}", preview.scroll_y()),
        None => "none".into(),
    }
}

/// The middle of the row reading `text` in the menu hung off `host`, on the window's surface,
/// which sits at the screen's origin under Xvfb: the menu's own surface is placed on it.
fn menu_item(host: &gtk::Widget, text: &str) -> Option<(f64, f64)> {
    fn label(widget: &gtk::Widget, text: &str) -> Option<gtk::Widget> {
        std::iter::successors(widget.first_child(), |w| w.next_sibling()).find_map(|w| {
            match w.downcast_ref::<gtk::Label>() {
                Some(l) if l.label() == text => Some(w),
                _ => label(&w, text),
            }
        })
    }
    let popover = std::iter::successors(host.first_child(), |w| w.next_sibling())
        .find_map(|w| w.downcast::<gtk::Popover>().ok())?;
    let b = label(popover.upcast_ref(), text)?.compute_bounds(&popover)?;
    let popup = popover.surface()?.downcast::<gtk::gdk::Popup>().ok()?;
    let (sx, sy) = popover.surface_transform();
    Some((
        f64::from(popup.position_x()) + sx + f64::from(b.x() + b.width() / 2.0),
        f64::from(popup.position_y()) + sy + f64::from(b.y() + b.height() / 2.0),
    ))
}

/// A pause for a screenshot of the note and the PDF, which are the two looks worth comparing.
async fn shot(kind: &str, case: &str) {
    if kind == "note" || kind == "pdf" {
        println!("bench present shot {case}");
        glib::timeout_future(Duration::from_millis(1500)).await;
    }
}

/// The status bar's height and the toast's bottom every 25 ms for the first 300 ms of the bar
/// coming up or going, for the note: the toast must clear the bar on every frame.
async fn frames(app: &Rc<App>, kind: &str, way: &str) {
    if kind != "note" {
        return;
    }
    let mut seen = Vec::new();
    for _ in 0..12 {
        glib::timeout_future(Duration::from_millis(25)).await;
        let bottom = toast_bottom(app).map_or(-1.0, f32::round);
        seen.push((app.toolbar.bottom_bar_height(), bottom));
    }
    let clear = seen
        .iter()
        .all(|(bar, bottom)| *bottom <= (app.window.height() - bar) as f32);
    println!("bench present {kind}_toast_{way} clear={clear} frames={seen:?}");
}

/// Where `widget` is drawn in the window, or `hidden`.
fn drawn(app: &App, widget: &gtk::Widget) -> String {
    match (widget.is_drawable(), widget.compute_bounds(&app.window)) {
        (true, Some(b)) => format!(
            "{:.0},{:.0}+{:.0}x{:.0}",
            b.x(),
            b.y(),
            b.width(),
            b.height()
        ),
        (true, None) => "drawn".into(),
        (false, _) => "hidden".into(),
    }
}

/// What the window shows: the active pane's tab in front and the preview, the rest of the panes
/// and their tab bars, the sidebar, and a PDF's zoom.
fn screen(app: &Rc<App>) -> String {
    let pane = app.pane();
    let front = pane.tabs.selected_page();
    let preview = app
        .preview
        .borrow()
        .as_ref()
        .map_or("none".into(), |p| drawn(app, p.widget()));
    let panes = app.panes.borrow();
    let others = panes
        .iter()
        .filter(|p| !Rc::ptr_eq(p, &pane) && p.widget().is_drawable())
        .count();
    let bars = panes.iter().filter(|p| p.bar.is_drawable()).count();
    format!(
        "front={:?} page={} preview={preview} other_panes={others} tab_bars={bars} sidebar={} \
         header={} pdf_zoom={:?} focus={:?}",
        front.as_ref().map(|p| p.title().to_string()),
        front.map_or("none".into(), |p| drawn(app, &p.child())),
        app.sidebar_column.is_drawable(),
        app.toolbar.reveals_top_bars(),
        app.active_pdf().and_then(|pdf| pdf.zoom_label()),
        gtk::prelude::GtkWindowExt::focus(&app.window).map(|f| f.type_().name()),
    )
}

/// The toast over the window, the status bar and whether the two overlap.
fn toast_and_bar(app: &Rc<App>) -> String {
    let bottom = toast_bottom(app);
    let bar = app.statusbar.widget();
    let top = bar.compute_bounds(&app.window).map(|b| b.y());
    let shown = app.toolbar.bottom_bar_height() > 0;
    let overlap = matches!((bottom, top), (Some(b), Some(t)) if shown && b > t);
    format!(
        "toast_bottom={:?} bar_top={:?} bar_shown={shown} overlap={overlap} window_h={}",
        bottom.map(|b| b.round()),
        top.map(|t| t.round()),
        app.window.height()
    )
}

/// Where the toasts up over the window end, in the window: the bottom of their pile, the
/// overlay's last child.
fn toast_bottom(app: &Rc<App>) -> Option<f32> {
    let bounds = app
        .toasts
        .widget()
        .last_child()?
        .compute_bounds(&app.window)?;
    Some(bounds.y() + bounds.height())
}

/// What leaving F5 has to put back: every divider in the window, the sidebar, and what has the
/// keyboard.
fn layout(app: &Rc<App>) -> String {
    fn dividers(widget: &gtk::Widget, out: &mut Vec<i32>) {
        if let Some(paned) = widget.downcast_ref::<gtk::Paned>() {
            out.push(paned.position());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            dividers(&c, out);
            child = c.next_sibling();
        }
    }
    let mut positions = Vec::new();
    dividers(app.window.upcast_ref(), &mut positions);
    let focus = gtk::prelude::GtkWindowExt::focus(&app.window);
    let editor = app.active().is_some_and(|tab| {
        focus
            .as_ref()
            .is_some_and(|f| f == tab.view.upcast_ref::<gtk::Widget>())
    });
    let panes: Vec<String> = app
        .panes
        .borrow()
        .iter()
        .map(|p| drawn(app, p.widget()))
        .collect();
    let preview = app
        .preview
        .borrow()
        .as_ref()
        .map_or("hidden".into(), |p| drawn(app, p.widget()));
    format!(
        "window={}x{} dividers={positions:?} panes={panes:?} preview={preview} sidebar={} \
         tab_bars={} focus={:?} editor_focused={editor}",
        app.window.width(),
        app.window.height(),
        app.sidebar_column.is_visible(),
        app.panes
            .borrow()
            .iter()
            .filter(|p| p.bar.is_drawable())
            .count(),
        focus.map(|f| f.type_().name()),
    )
}
