//! Presentation mode's drill: what F5 puts on screen for each kind of tab, what still works over
//! it, and what leaving it puts back.

use super::*;
use webkit6::prelude::WebViewExt;

/// `ACCENT_BENCH_CHROME=present:<note>,<pdf>,<image>,<side>` lays out two panes, the note, the
/// PDF, the image and a shell on the left and `<side>` on the right, and presents each of the
/// four from the left pane in turn. For each it prints:
///
/// - `<kind>`: what is on screen, the tab's own page and the preview (drawn or `hidden`, with
///   their bounds in the window), how many other panes and tab bars are drawn, the sidebar, a
///   PDF's zoom and what has the keyboard;
/// - `<kind>_after`: the layout once that F5 is left again, every divider, the panes, the
///   preview, the sidebar and the keyboard, `same=` saying whether it is the one before it;
///
/// and, presenting it again:
///
/// - `<kind>_find`: whether Ctrl+F opened a bar, inside the presented pane and drawn;
/// - `<kind>_card`: a `Ctrl+Tab` held past the card's delay: whether the card is drawn, inside
///   the window, and what the step put on screen;
/// - `<kind>_escape`: Escape over the held chord, as the window hears it: the tab it started from
///   in front again, and still presenting;
/// - `<kind>_toast` and `<kind>_toast_away`: a toast up with the pointer on the status bar's strip
///   and once it has left, the toast's bottom against the bar's top; for the note,
///   `note_toast_up` and `note_toast_down` follow both on the frames between.
///
/// Then `split` and `split_after`: the note alone in Split view, its divider off the middle,
/// presented and left again. Last, presenting again, real keys: a held `Ctrl+Tab` pressed twice
/// and Escape before Ctrl comes up, then one held and let go, every change of the chord, the card
/// and what is presented printing as `bench present keys`; Page Down over the presented note and
/// PDF (`<kind>_page_down`, the note's `scrollY` or the PDF's page before and after); the status
/// bar's menus over the PDF by pointer, `bar_menu_<step>` saying whether a menu is up, the bar
/// with it and the zoom: the zoom readout right-clicked (`open`), the pointer up over the page
/// (`away`), Fit Width picked (`pick`, the bar gone with the pointer off it), the page count
/// clicked (`page`) and Escape (`escape`, the bar staying under the pointer); and F5 and Escape
/// pressed in the presented shell (`shell_F5`, `shell_Escape`, both to read
/// `presenting=false`). `bench present xtest <steps>` asks for those steps, under Xvfb
/// `build-aux/xtest.py :N "<steps>"`, the first of them giving the window the X input focus.
/// `bench present shot <case>` is a second and a half held for a screenshot.
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
            let card = pane.switcher.widget();
            let inside = card.compute_bounds(&app.window).is_some_and(|b| {
                b.x() >= 0.0
                    && b.y() >= 0.0
                    && b.x() + b.width() <= app.window.width() as f32
                    && b.y() + b.height() <= app.window.height() as f32
            });
            println!(
                "bench present {kind}_card shown={} card={} inside={inside} {}",
                pane.switcher.shown(),
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
                pane.switcher.shown(),
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

        // Split view: the preview leaves its place beside the panes for the presented one, and
        // comes back to it with its divider where it was. One pane, so the divider has room to be
        // off the middle in a window this size.
        if let Some(side) = page(&rels[3]) {
            app.close_page(&side);
        }
        app.set_mode(Mode::Split);
        if let Some(note) = page(&rels[0]) {
            app.reveal_page(&note);
        }
        glib::timeout_future(Duration::from_millis(800)).await;
        app.paned.set_position(app.paned.width() * 3 / 5);
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
        for _ in 0..100 {
            if app.window.is_active() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        glib::timeout_future(Duration::from_millis(500)).await;
        println!(
            "bench present xtest keydown ctrl; key Tab; key Tab; sleep 0.6; key Escape; keyup ctrl; \
             sleep 0.5; keydown ctrl; key Tab; sleep 0.6; keyup ctrl"
        );
        let mut last = String::new();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(6) {
            let pane = app.pane();
            let state = format!(
                "held={} card={} presenting={} {}",
                pane.cycling().is_some(),
                drawn(&app, pane.switcher.widget()),
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

/// How far the presented note is scrolled, or which page of the presented PDF is in view.
async fn position(app: &Rc<App>) -> String {
    if let Some(pdf) = app.active_pdf() {
        return pdf.page_label().unwrap_or_default();
    }
    let view = app.preview.borrow().as_ref().map(|p| p.view().clone());
    let Some(view) = view else {
        return "none".into();
    };
    view.evaluate_javascript_future("scrollY", None, None)
        .await
        .map_or("?".into(), |y| format!("scrollY={}", y.to_double()))
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
