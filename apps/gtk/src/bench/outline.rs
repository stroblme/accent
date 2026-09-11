//! The Outline pane following the caret.

use super::*;

/// Lines of the note the caret is walked to: above the first heading, down through the sections,
/// back up to the top one and down again, so the tab switch after it has a row far from the top to
/// come back to. Clamped to the note's length.
const WALK: [i32; 7] = [0, 6, 60, 120, 400, 12, 190];

/// Open `note` with the Outline pane showing, walk its caret over [`WALK`] and print what the pane
/// selected each time; then open `other` and come back, then move the caret while the pane is
/// hidden and show it again. `hold:<note>` instead prints where the editor and the list are and
/// then the pane's state whenever it changes, for 20 s, for keys and a pointer driven by XTEST.
pub(super) fn bench_outline(app: &Rc<App>, arg: &str) {
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
        _ => return bench_quit(&app),
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

/// The caret's line, the heading it is under, the row the pane selected and whether that row is
/// on screen, and who has the keyboard. `placed` is where the caret was put: a row activated by
/// the selection would have moved it to that heading.
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
