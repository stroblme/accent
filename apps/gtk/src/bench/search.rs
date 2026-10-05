//! The Search pane drill: whether its rows follow the vault under a query that is already up.

use super::*;

/// The files the drill writes and takes away again, so it rewrites nothing of the reader's.
const NOTE: &str = "accent-bench-search";
/// One watcher debounce (300 ms), the index batch behind it and the pane's own settle, with room
/// for a large vault's worker to get to it.
const SETTLE: Duration = Duration::from_millis(2500);
/// How long to wait for a cold vault's first walk before giving up on it. `testvault` is 40k
/// files and takes about half a minute.
const INDEXED: Duration = Duration::from_secs(120);
/// One keystroke of the `type:` and `walk:` drills: faster than the pane's wait, as typing is.
const TYPING: Duration = Duration::from_millis(100);
/// How often those drills look at the rows, to time each change from the last keystroke.
const LOOK: Duration = Duration::from_millis(2);
/// How long after the last keystroke they wait, at most, for the queries still running to land.
const DRAIN: Duration = Duration::from_secs(30);

/// `ACCENT_BENCH_SEARCH=<query>[:<n>]` puts `<query>` in the Search pane, then writes `n` files
/// holding it twice — one note unless the tail says otherwise, every second file a `.txt` rather
/// than a note — and takes them away again, printing what the pane lists at each step and what
/// the Replace All button says. The whole cycle runs twice: once under ranked full text, and once
/// with the replace row open, which is the exact scan Replace All needs.
///
/// `before` and `removed` must match, and `added` must have `2n` rows more: a row is a match and
/// not a file, in either mode. That is the pane following the vault without the box being
/// touched. Pass a word the vault does not already hold and the steps read `rows=0`, `rows=2n`,
/// `rows=0` in both modes.
///
/// The button counts only under the exact scan, where it must say what the rewrite would touch:
/// with such a word, `Replace All (0)` at `grep`, `grep_removed` and `away_removed`, and
/// `Replace All (2n)` at `grep_added` and `away_added`, the `.txt` files included — Replace All
/// rewrites every indexed file, not only the notes. The ranked steps read `Replace All (0)`, not
/// pressable.
///
/// The last two steps stage the same batch with the *Files* pane in front instead, so
/// `away_added` and `away_removed` are the catch-up a pane that was not on screen owes when it
/// comes back: `rows=2n` and `rows=0` again, not the rows it was left with.
///
/// `RUST_LOG=accent=debug` prints `sidebar query … grep=… ms=…` for every query the run makes, so
/// the `grep=true` lines after the `grep` step are what a requery nobody asked for costs in exact
/// mode, and `<n>` is how many files the settled batch behind one of them touched.
///
/// `ACCENT_BENCH_SEARCH=type:<query>` is a different drill: it types `<query>` a character at a
/// time, faster than the pane waits for, and prints the rows each time they change, with the
/// milliseconds since the last keystroke: first the prefix rows, then the mid-word rows appended
/// below them once the typing has stopped; `last_key running=` is how many queries for the
/// prefixes typed before were still on worker threads at the last keystroke, and `settled` when
/// the rows last changed and when nothing was running any more (`drained_ms`). `=walk:<query>`
/// does the same with All on, where the walk past the index appends its rows last, under
/// `Not Indexed`, and prints them once more with All off again (`all_off`). `RUST_LOG=accent=debug` prints one `sidebar pass` line per pass for
/// the whole query, not one per character.
pub(super) fn bench_search(app: &Rc<App>, arg: &str) {
    if let Some(rel) = arg.strip_prefix("seed:") {
        return bench_seed(app, rel);
    }
    if let Some(query) = arg.strip_prefix("label:") {
        // `:xml` saves the diagram as a plain `.xml`, known for one by its first element alone.
        let (query, ext) = match query.strip_suffix(":xml") {
            Some(query) => (query.to_string(), "xml"),
            None => (query.to_string(), "drawio"),
        };
        return bench_search_indexed(app.clone(), Instant::now(), move |app| {
            bench_label(app, query, ext)
        });
    }
    let more = (arg.strip_prefix("more:").map(|rest| (rest, false)))
        .or_else(|| arg.strip_prefix("click:").map(|rest| (rest, true)));
    if let Some((rest, click)) = more {
        let (query, lines) = match rest.rsplit_once(':') {
            Some((query, n)) if n.parse::<usize>().is_ok() => (query, n.parse().unwrap_or(12)),
            _ => (rest, 12),
        };
        let query = query.to_string();
        return bench_search_indexed(app.clone(), Instant::now(), move |app| {
            bench_more(app, query, lines, click)
        });
    }
    let typed = arg
        .strip_prefix("type:")
        .map(|query| (query, false))
        .or_else(|| arg.strip_prefix("walk:").map(|query| (query, true)));
    if let Some((query, all)) = typed {
        let typed: Vec<String> = (1..=query.chars().count())
            .map(|n| query.chars().take(n).collect())
            .collect();
        return bench_search_indexed(app.clone(), Instant::now(), move |app| {
            if let Some(sidebar) = app.sidebar.get() {
                sidebar.show_pane("search");
                if all {
                    sidebar.toggle_search_all();
                }
            }
            // Typed once the window's first git refreshes are in: each one that lands asks the
            // question again, which would put a second answer into what is being timed.
            glib::timeout_add_local_once(SETTLE, move || bench_type(app, typed, 0, all));
        });
    }
    // A `:<n>` tail stages a batch of several files, which is what a sync pull looks like; a
    // query with no such tail is the one note an autosave writes.
    let (query, count) = match arg
        .rsplit_once(':')
        .and_then(|(q, n)| Some((q, n.parse::<usize>().ok()?)))
    {
        Some((query, count)) => (query.to_string(), count.max(1)),
        None => (arg.to_string(), 1),
    };
    bench_search_indexed(app.clone(), Instant::now(), move |app| {
        bench_search_run(app, query, count)
    });
}

/// Ctrl+Shift+F and Ctrl+Shift+H against what the find bar's Ctrl+F and Ctrl+H do: the box that
/// takes the keyboard holds the editor's selection where there is one, and has all it holds
/// selected, so what is typed next replaces it. Fired as the chords' actions over the note at
/// `rel`, each printing which box has the keyboard and what it has selected, at once and again
/// once the query's delayed search has run. Last, `bench seed focus_window` and then
/// `bench seed_ready <steps>` for `build-aux/xtest.py :N` to run, the real chord and two letters,
/// and what the box then holds. `Tab::set_text` leaves the tab clean and the note's own text goes
/// back at the end, so nothing is written.
fn bench_seed(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(600)).await;
        let (Some(tab), Some(boxes)) = (
            app.active(),
            app.sidebar.get().and_then(|s| s.search_boxes()),
        ) else {
            println!("bench seed no_tab");
            return bench_quit(&app);
        };
        let show = |step: &str| {
            let focus = gtk::prelude::GtkWindowExt::focus(&app.window);
            let has = |entry: &gtk::Editable| {
                focus
                    .as_ref()
                    .is_some_and(|f| f == entry.upcast_ref::<gtk::Widget>() || f.is_ancestor(entry))
            };
            let at = match boxes.iter().position(has) {
                Some(0) => "query",
                Some(_) => "replace",
                None => "elsewhere",
            };
            let selected = boxes
                .iter()
                .find(|entry| has(entry))
                .map(|entry| (entry.text().to_string(), entry.selection_bounds()));
            println!("bench seed {step} focus={at} {selected:?}");
        };
        let press = |action: &str| {
            let _ = WidgetExt::activate_action(&app.window, action, None);
        };
        let settle = || glib::timeout_future(Duration::from_millis(500));
        // A query left in the box and nothing selected in the note: the box keeps it, selected.
        if let Some(sidebar) = app.sidebar.get() {
            sidebar.set_search_text("old query");
        }
        tab.view.grab_focus();
        tab.buffer.place_cursor(&tab.buffer.start_iter());
        press("win.pane-search");
        show("f");
        settle().await;
        show("f_settled");
        // Again with the box already holding the keyboard, its caret at the end of the text.
        // Pressed a moment later, as a hand would: a deselect and a select in one turn of the
        // main loop hand the X primary selection over twice, and GTK takes the box's selection
        // away when the first handover lands.
        boxes[0].select_region(-1, -1);
        settle().await;
        press("win.pane-search");
        settle().await;
        show("f_again");
        // A selection in the note is the query, selected.
        let own = tab.text();
        tab.set_text("alpha beta gamma\n");
        tab.view.grab_focus();
        let (start, end) = (tab.buffer.iter_at_offset(6), tab.buffer.iter_at_offset(10));
        tab.buffer.select_range(&start, &end);
        press("win.pane-search");
        settle().await;
        show("f_selection");
        // Ctrl+H with a query: the replacement box, all it holds selected.
        boxes[1].set_text("new");
        tab.view.grab_focus();
        press("win.replace-in-files");
        settle().await;
        show("h");
        // The real chord and the real keys after it, once the window has the X input focus
        // (`build-aux/xtest.py :N "move 700 400; focus"`): what is typed replaces the query.
        println!("bench seed focus_window");
        for _ in 0..50 {
            if app.window.is_active() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        tab.view.grab_focus();
        tab.buffer.place_cursor(&tab.buffer.start_iter());
        settle().await;
        println!("bench seed_ready key ctrl+shift+f; type zz");
        glib::timeout_future(Duration::from_millis(1500)).await;
        println!("bench seed typed {:?}", boxes[0].text());
        tab.set_text(&own);
        bench_quit(&app);
    });
}

/// Nothing can be searched for until the first walk is over, and on a large vault that is not
/// the 400 ms every other drill starts after.
fn bench_search_indexed(app: Rc<App>, since: Instant, then: impl FnOnce(Rc<App>) + 'static) {
    if !app.reconciled.get() {
        if since.elapsed() > INDEXED {
            println!("bench search indexed=false");
            return bench_quit(&app);
        }
        glib::timeout_add_local_once(Duration::from_millis(200), move || {
            bench_search_indexed(app, since, then)
        });
        return;
    }
    then(app);
}

fn bench_search_run(app: Rc<App>, query: String, count: usize) {
    let Some(sidebar) = app.sidebar.get() else {
        return bench_quit(&app);
    };
    sidebar.show_pane("search");
    sidebar.set_search_text(&query);
    glib::timeout_add_local_once(SETTLE, move || {
        bench_search_print(&app, "before");
        bench_search_cycle(app, query, count, "", |app, query, count| {
            // The exact scan, for the half of the cost that is not an FTS lookup — and then the
            // same batch again under it, which is the requery the reader did not ask for.
            if let Some(sidebar) = app.sidebar.get() {
                sidebar.show_replace();
            }
            glib::timeout_add_local_once(SETTLE, move || {
                bench_search_print(&app, "grep");
                bench_search_cycle(app, query, count, "grep_", |app, query, count| {
                    // And last, the same batch again with another pane in front of this one.
                    let paths = bench_search_paths(&app, count);
                    let gone = paths.clone();
                    bench_search_away(
                        app,
                        "away_added",
                        move || bench_search_write(&paths, &query),
                        move |app| {
                            bench_search_away(
                                app,
                                "away_removed",
                                move || bench_search_remove(&gone),
                                |app| bench_quit(&app),
                            )
                        },
                    );
                });
            });
        });
    });
}

/// Write the marker files behind the pane's back — which is what an edit in another editor is, the
/// vault's own watcher being what has to carry it — then take them away again, printing the rows
/// after each. `mode` names which mode the pane was in, and `then` is what the run does next.
fn bench_search_cycle(
    app: Rc<App>,
    query: String,
    count: usize,
    mode: &'static str,
    then: impl FnOnce(Rc<App>, String, usize) + 'static,
) {
    let paths = bench_search_paths(&app, count);
    bench_search_write(&paths, &query);
    glib::timeout_add_local_once(SETTLE, move || {
        bench_search_print(&app, &format!("{mode}added"));
        bench_search_remove(&paths);
        glib::timeout_add_local_once(SETTLE, move || {
            bench_search_print(&app, &format!("{mode}removed"));
            then(app, query, count);
        });
    });
}

/// One half of the catch-up: leave the Search pane for the Files pane, move the vault under it
/// with `act`, come back, and print what the rows say once it has answered for the change it
/// never saw. Without the pane's dirty flag these are the rows it had when it was left.
fn bench_search_away(
    app: Rc<App>,
    step: &'static str,
    act: impl FnOnce() + 'static,
    then: impl FnOnce(Rc<App>) + 'static,
) {
    let Some(sidebar) = app.sidebar.get() else {
        return bench_quit(&app);
    };
    sidebar.show_pane("files");
    act();
    glib::timeout_add_local_once(SETTLE, move || {
        if let Some(sidebar) = app.sidebar.get() {
            sidebar.show_pane("search");
        }
        // The query the switch starts runs on a worker thread like any other, so the rows are
        // read a moment later rather than in the same turn of the main loop.
        glib::timeout_add_local_once(SETTLE, move || {
            bench_search_print(&app, step);
            then(app);
        });
    });
}

/// Where the marker files go: one per file of the batch the drill stages, every second one a
/// `.txt`, which the exact scan and Replace All reach just as they reach a note.
fn bench_search_paths(app: &Rc<App>, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|i| {
            let ext = if i % 2 == 1 { "txt" } else { "md" };
            app.root().join(format!("{NOTE}-{i}.{ext}"))
        })
        .collect()
}

fn bench_search_write(paths: &[PathBuf], query: &str) {
    for path in paths {
        // Twice, on two lines: a row is a match and not a file, so each file written here is two
        // rows in both modes.
        let body = format!("a line holding {query} once\nand a second line holding {query}\n");
        if let Err(e) = std::fs::write(path, body) {
            println!("bench search wrote=false {e}");
        }
    }
}

fn bench_search_remove(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

/// One keystroke of the `walk:` drill, and once the query is typed out, its three steps.
fn bench_type(app: Rc<App>, typed: Vec<String>, i: usize, all: bool) {
    let (Some(sidebar), Some(text)) = (app.sidebar.get(), typed.get(i)) else {
        return bench_quit(&app);
    };
    sidebar.set_search_text(text);
    if i + 1 < typed.len() {
        glib::timeout_add_local_once(TYPING, move || bench_type(app, typed, i + 1, all));
        return;
    }
    let typed_at = Instant::now();
    // Queries for prefixes the box has moved past that are still on a worker thread.
    println!("bench search last_key running={}", sidebar.search_running());
    let seen = RefCell::new(sidebar.search_state());
    // When the rows last changed, and since when nothing has been running.
    let (changed, drained) = (Cell::new(0), Cell::new(None));
    glib::timeout_add_local(LOOK, move || {
        let state = app.sidebar.get().map(|s| s.search_state());
        if let Some(state) = state.filter(|state| *state != *seen.borrow()) {
            changed.set(typed_at.elapsed().as_millis());
            bench_type_print(changed.get(), &state);
            seen.replace(state);
        }
        let running = app.sidebar.get().map_or(0, |s| s.search_running());
        if running > 0 {
            drained.set(None);
        } else if drained.get().is_none() {
            drained.set(Some(typed_at.elapsed().as_millis()));
        }
        if typed_at.elapsed() < SETTLE || (running > 0 && typed_at.elapsed() < DRAIN) {
            return glib::ControlFlow::Continue;
        }
        println!(
            "bench search settled final_ms={} drained_ms={:?} running={running}",
            changed.get(),
            drained.get()
        );
        let Some(sidebar) = app.sidebar.get().filter(|_| all) else {
            bench_quit(&app);
            return glib::ControlFlow::Break;
        };
        sidebar.toggle_search_all();
        let app = app.clone();
        glib::timeout_add_local_once(SETTLE, move || {
            if let Some(sidebar) = app.sidebar.get() {
                let (page, rows, count) = sidebar.search_state();
                println!("bench search step=all_off page={page} count={count:?} rows={rows:?}");
            }
            bench_quit(&app);
        });
        glib::ControlFlow::Break
    });
}

/// One change of the rows: every name while they fit on a line, the first and last few past that.
fn bench_type_print(ms: u128, (page, rows, count): &(String, Vec<String>, String)) {
    let names = match rows.len() {
        0..=12 => format!("{rows:?}"),
        n => format!("{:?} … {:?}", &rows[..6], &rows[n - 4..]),
    };
    println!(
        "bench search ms={ms} page={page} count={count:?} rows={} {names}",
        rows.len()
    );
}

fn bench_search_print(app: &Rc<App>, step: &str) {
    let Some(sidebar) = app.sidebar.get() else {
        return println!("bench search step={step} pane=none");
    };
    let (page, rows, count) = sidebar.search_state();
    let rows = rows.len();
    let (button, sensitive) = sidebar.replace_all_state();
    println!(
        "bench search step={step} page={page} rows={rows} count={count:?} button=\"{button}\" \
         sensitive={sensitive}"
    );
}

/// Files beside the one `=more:` opens, each holding the query twice, so the list scrolls.
const MORE_BESIDE: usize = 30;
/// How long `=click:` waits for the press.
const CLICK_WAIT: Duration = Duration::from_secs(20);

/// The note `=more:` opens: named to sort among the others, so the exact scan, which lists in
/// path order, puts it in the middle of the list.
/// `ACCENT_BENCH_SEARCH=label:<query>` writes a two-page diagram holding `<query>` in a bold
/// label on its second page, searches for it and prints each row as its name and dim line, which
/// must name the page (`Second`), not a line; then opens the first row as a click does and prints
/// the page the diagram shows and what is selected, which must be `page=1` and `["found"]`. Once
/// ranked, then with the replace row open. `=label:<query>:xml` saves it as a plain `.xml`.
fn bench_label(app: Rc<App>, query: String, ext: &'static str) {
    let rel = format!("{NOTE}.{ext}");
    let path = app.root().join(&rel);
    let cell = |id: &str, label: &str| {
        format!(
            r#"<mxCell id="{id}" value="{label}" style="html=1;" vertex="1" parent="1"><mxGeometry x="40" y="40" width="160" height="60" as="geometry"/></mxCell>"#
        )
    };
    let page = |name: &str, cells: &str| {
        format!(
            r#"<diagram name="{name}" id="{name}"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>{cells}</root></mxGraphModel></diagram>"#
        )
    };
    let xml = format!(
        "<mxfile>{}{}</mxfile>",
        page("First", &cell("other", "nothing here")),
        page(
            "Second",
            &cell("found", &format!("&lt;b&gt;{query}&lt;/b&gt; label"))
        )
    );
    if let Err(e) = std::fs::write(&path, xml) {
        println!("bench search label wrote=false {e}");
        return bench_quit(&app);
    }
    let Some(sidebar) = app.sidebar.get() else {
        return bench_quit(&app);
    };
    sidebar.show_pane("search");
    sidebar.set_search_text(&query);
    glib::timeout_add_local_once(SETTLE, move || {
        bench_label_open(&app, "ranked");
        let app2 = app.clone();
        glib::timeout_add_local_once(SETTLE, move || {
            bench_label_opened(&app2, "ranked");
            if let Some(sidebar) = app2.sidebar.get() {
                sidebar.show_replace();
            }
            glib::timeout_add_local_once(SETTLE, move || {
                bench_label_open(&app2, "exact");
                glib::timeout_add_local_once(SETTLE, move || {
                    bench_label_opened(&app2, "exact");
                    bench_search_remove(&[path]);
                    bench_quit(&app2);
                });
            });
        });
    });
}

/// Print the rows as they are drawn, name and dim line, and open the first as a click does.
fn bench_label_open(app: &Rc<App>, mode: &str) {
    let Some(view) = app.sidebar.get().and_then(|s| s.search_view()) else {
        return;
    };
    let label = |w: Option<gtk::Widget>| w.and_downcast::<gtk::Label>().map(|l| l.text());
    let mut rows = Vec::new();
    let mut child = view.first_child();
    while let Some(item) = child {
        if let Some(head) = item.first_child().and_then(|row| row.first_child()) {
            let name = label(head.first_child().and_then(|i| i.next_sibling()));
            rows.push(format!(
                "{} | {}",
                name.unwrap_or_default(),
                label(head.last_child()).unwrap_or_default()
            ));
        }
        child = item.next_sibling();
    }
    println!("bench search label={mode} rows={rows:?}");
    view.emit_by_name::<()>("activate", &[&0u32]);
}

/// What the diagram the row opened shows.
fn bench_label_opened(app: &Rc<App>, mode: &str) {
    match app.active_diagram() {
        Some(tab) => println!(
            "bench search label={mode} opened page={} selection={:?}",
            tab.page_index(),
            tab.selection()
        ),
        None => println!("bench search label={mode} opened=none"),
    }
}

fn more_note() -> String {
    format!("{NOTE}-15more.md")
}

/// `ACCENT_BENCH_SEARCH=more:<query>[:<n>]` writes a note holding `<query>` on `n` lines (12
/// unless the tail says otherwise) — five rows and a "+N more in this file" under them — beside
/// thirty files holding it twice, so the list scrolls, puts the tail row in the middle of the
/// list and opens it the way a click does. Once ranked, then with the replace row open.
///
/// Each prints `before` and `after`: the rows, the count line, the tabs open, the list's scroll,
/// how far down the list the tail row sat and the row now in its place sits, and which row is
/// selected. `after` must list `rows` + N − 1 (at most 100 more, and a tail row again past
/// that), with the count, the tabs, the scroll, the place and the selection as they were. Then
/// `requery` asks the same question again, as a change in the vault does, and must list the file
/// open still; `reset` asks for the query in capitals, a new question with the same matches, and
/// must list it shut.
///
/// `=click:` is the same drill with a real press: it prints `aim <x> <y>`, where the tail row is
/// on the screen, and waits 20 s for `build-aux/xtest.py <display> "move <x> <y>; down; up"`.
fn bench_more(app: Rc<App>, query: String, lines: usize, click: bool) {
    let Some(sidebar) = app.sidebar.get() else {
        return bench_quit(&app);
    };
    let paths = bench_search_paths(&app, MORE_BESIDE);
    bench_search_write(&paths, &query);
    let more = app.root().join(more_note());
    let body: String = (1..=lines)
        .map(|i| format!("line {i} holds {query}\n"))
        .collect();
    if let Err(e) = std::fs::write(&more, body) {
        println!("bench search wrote=false {e}");
    }
    sidebar.show_pane("search");
    sidebar.set_search_text(&query);
    glib::timeout_add_local_once(SETTLE, move || {
        bench_more_open(app, query, "ranked", click, move |app, query| {
            if let Some(sidebar) = app.sidebar.get() {
                sidebar.set_search_text(&query);
                sidebar.show_replace();
            }
            glib::timeout_add_local_once(SETTLE, move || {
                bench_more_open(app, query, "exact", click, move |app, _| {
                    bench_search_remove(&paths);
                    bench_search_remove(&[more]);
                    bench_quit(&app);
                })
            });
        })
    });
}

/// One mode of `=more:`: centre the tail row, open it, and print what the list did.
fn bench_more_open(
    app: Rc<App>,
    query: String,
    mode: &'static str,
    click: bool,
    then: impl FnOnce(Rc<App>, String) + 'static,
) {
    let (Some(sidebar), Some(view)) = (
        app.sidebar.get(),
        app.sidebar.get().and_then(|s| s.search_view()),
    ) else {
        return bench_quit(&app);
    };
    let (_, rows, _) = sidebar.search_state();
    let Some(tail) = rows.iter().position(|r| r.starts_with('+')) else {
        println!("bench search more={mode} tail=none rows={}", rows.len());
        return then(app, query);
    };
    let tail_label = rows[tail].clone();
    view.scroll_to(tail as u32, gtk::ListScrollFlags::NONE, None);
    // A frame or two for the list to lay itself out at each new scroll.
    glib::timeout_add_local_once(Duration::from_millis(300), move || {
        if let (Some(adj), Some(y)) = (view.vadjustment(), bench_row_y(&view, "", &tail_label)) {
            let top = (adj.value() + y - adj.page_size() / 2.0).max(0.0);
            adj.set_value(top.min(adj.upper() - adj.page_size()));
        }
        if let Some(selection) = view.model().and_downcast::<gtk::SingleSelection>() {
            selection.set_selected(tail as u32);
        }
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            bench_more_activate(app, view, query, mode, tail, click, then)
        });
    });
}

/// `=more:`'s tail row, centred: print the list, open the row, and print it again.
fn bench_more_activate(
    app: Rc<App>,
    view: gtk::ListView,
    query: String,
    mode: &'static str,
    tail: usize,
    click: bool,
    then: impl FnOnce(Rc<App>, String) + 'static,
) {
    let Some(sidebar) = app.sidebar.get() else {
        return bench_quit(&app);
    };
    let (_, rows, _) = sidebar.search_state();
    let (seen, tail_label) = (rows.len(), rows[tail].clone());
    let file = more_note();
    bench_more_print(&app, &view, mode, "before", ("", &tail_label));
    let tabs = app.open_tabs().len();
    let asked = Instant::now();
    let wait = match click {
        true => {
            match bench_row_aim(&view, &tail_label) {
                Some((x, y)) => println!("bench search more={mode} aim {x:.0} {y:.0}"),
                None => println!("bench search more={mode} aim=none"),
            }
            CLICK_WAIT
        }
        false => {
            view.emit_by_name::<()>("activate", &[&(tail as u32)]);
            SETTLE
        }
    };
    // Taken once, by the look that sees the rows change.
    let mut next = Some((then, tail_label));
    glib::timeout_add_local(LOOK, move || {
        let now = app.sidebar.get().map_or(seen, |s| s.search_state().1.len());
        if now == seen && asked.elapsed() < wait {
            return glib::ControlFlow::Continue;
        }
        let Some((then, tail_label)) = next.take() else {
            return glib::ControlFlow::Break;
        };
        println!(
            "bench search more={mode} opened_ms={} tabs_opened={}",
            asked.elapsed().as_millis(),
            app.open_tabs().len() - tabs
        );
        let (app, view, query, file) = (app.clone(), view.clone(), query.clone(), file.clone());
        // Printed a moment later, once the list has laid the new rows out.
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            bench_more_print(&app, &view, mode, "after", (&file, "line 6"));
            if let Some(sidebar) = app.sidebar.get() {
                sidebar.requery_search();
            }
            glib::timeout_add_local_once(SETTLE, move || {
                bench_more_print(&app, &view, mode, "requery", (&file, "line 6"));
                if let Some(sidebar) = app.sidebar.get() {
                    sidebar.set_search_text(&query.to_uppercase());
                }
                glib::timeout_add_local_once(SETTLE, move || {
                    bench_more_print(&app, &view, mode, "reset", ("", &tail_label));
                    then(app, query);
                });
            });
        });
        glib::ControlFlow::Break
    });
}

/// What `=more:` reads at each step; `at` names the row whose place on the list is printed, by
/// its name and its dim line.
fn bench_more_print(app: &Rc<App>, view: &gtk::ListView, mode: &str, step: &str, at: (&str, &str)) {
    let Some(sidebar) = app.sidebar.get() else {
        return;
    };
    let (page, rows, count) = sidebar.search_state();
    let tails: Vec<&String> = rows.iter().filter(|r| r.starts_with('+')).collect();
    let selected = view
        .model()
        .and_downcast::<gtk::SingleSelection>()
        .map(|s| s.selected());
    let scroll = view.vadjustment().map(|a| a.value());
    println!(
        "bench search more={mode} step={step} page={page} rows={} count={count:?} tabs={} \
         scroll={scroll:?} y={:?} {:?} selected={selected:?} tails={tails:?}",
        rows.len(),
        app.open_tabs().len(),
        bench_row_y(view, at.0, at.1),
        at.1,
    );
}

/// Where on the screen the middle of the tail row with the dim line `dir` is. With no window
/// manager under Xvfb the window's surface sits at the screen's corner, so that is the widget's
/// place in the surface.
fn bench_row_aim(view: &gtk::ListView, dir: &str) -> Option<(f64, f64)> {
    let y = bench_row_y(view, "", dir)?;
    let root = view.root()?;
    let native = view.native()?;
    let (sx, sy) = native.surface_transform();
    let at = view.compute_point(&root, &gtk::graphene::Point::new(40.0, y as f32 + 12.0))?;
    Some((f64::from(at.x()) + sx, f64::from(at.y()) + sy))
}

/// How far down the list's visible part the row named `name` with the dim line `dir` sits, or
/// `None` while it is not laid out. Each list child holds one row: the head box of icon, name and
/// dim line, above the snippet.
fn bench_row_y(view: &gtk::ListView, name: &str, dir: &str) -> Option<f64> {
    let label = |w: Option<gtk::Widget>| w.and_downcast::<gtk::Label>().map(|l| l.text());
    let mut child = view.first_child();
    while let Some(item) = child {
        let head = item.first_child().and_then(|row| row.first_child());
        if let Some(head) = head {
            let icon = head.first_child();
            let found = label(icon.and_then(|i| i.next_sibling())).as_deref() == Some(name)
                && label(head.last_child()).as_deref() == Some(dir);
            if found && item.is_child_visible() {
                return item.compute_bounds(view).map(|b| f64::from(b.y()));
            }
        }
        child = item.next_sibling();
    }
    None
}
