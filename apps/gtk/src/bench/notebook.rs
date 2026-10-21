//! `ACCENT_BENCH_NOTEBOOK=<ipynb>,<marimo .py>,<long ipynb>`: the notebook tab on the generated
//! vault's `Notebooks/` (a copy under /tmp: it saves). It prints how the first notebook's outputs
//! show, an edit of each kind with its undo and the save that follows, the view kept still by a
//! click and following the arrows, the marimo notebook kept as written, the long notebook's open
//! time, memory and editors built, the view kept still as more are built above what shows, and a
//! presented notebook's read-only. All under Dark, which recolours the pictures.
//! `ACCENT_BENCH_NOTEBOOK_SHOT=<png>` adds a picture of the window with the first notebook open,
//! for looking at rather than asserting on.

use super::*;
use crate::notebook::NotebookTab;

pub(super) fn bench_notebook(app: &Rc<App>, arg: &str) {
    scratch_only(app, "ACCENT_BENCH_NOTEBOOK");
    let rels: Vec<String> = arg.split(',').map(str::to_string).collect();
    let [ipynb, marimo, big] = &rels[..] else {
        eprintln!("ACCENT_BENCH_NOTEBOOK=<ipynb>,<marimo .py>,<long ipynb>");
        return bench_quit(app);
    };
    let (app, ipynb, marimo, big) = (app.clone(), ipynb.clone(), marimo.clone(), big.clone());
    glib::spawn_future_local(async move {
        crate::theme::apply(accent_core::config::Theme::Dark);
        app.restyle_all();
        open_drill(&app, &ipynb).await;
        edit_drill(&app, &ipynb).await;
        scroll_drill(&app).await;
        find_drill(&app, &ipynb).await;
        hit_drill(&app, &ipynb, &marimo).await;
        outline_drill(&app, &ipynb, &marimo).await;
        marimo_drill(&app, &marimo).await;
        big_drill(&app, &big).await;
        present_drill(&app).await;
        text_drill(&app, &ipynb).await;
        bench_quit(&app);
    });
}

/// Open `rel`, the time until its tab is there.
async fn opened(app: &Rc<App>, rel: &str) -> Option<(Rc<NotebookTab>, f64)> {
    let t = Instant::now();
    app.open_path(rel);
    for _ in 0..200 {
        if let Some(tab) = app.doc_for(rel).and_then(|d| d.notebook().cloned()) {
            return Some((tab, ms_since(t)));
        }
        glib::timeout_future(Duration::from_millis(5)).await;
    }
    println!("bench notebook no_tab {rel}");
    None
}

async fn open_drill(app: &Rc<App>, rel: &str) {
    let Some((tab, ms)) = opened(app, rel).await else {
        return;
    };
    // The pictures decode on workers and the formulas typeset in WebKit: given the time they take.
    for _ in 0..50 {
        glib::timeout_future(Duration::from_millis(100)).await;
        let (_, _, _, c) = tab.counts();
        if c.typeset == c.latex && c.recoloured == c.pictures {
            break;
        }
    }
    // A picture a screen at a time down the notebook: `<png>`, then `-1.png`, `-2.png`, ….
    if let Ok(path) = std::env::var("ACCENT_BENCH_NOTEBOOK_SHOT") {
        let mut shots = 0;
        for n in 0..8 {
            tab.bench_scroll(n);
            glib::timeout_future(Duration::from_millis(300)).await;
            bench_pump();
            let path = match n {
                0 => path.clone(),
                n => path.replace(".png", &format!("-{n}.png")),
            };
            shots += usize::from(super::diagram::shoot(
                app.window.upcast_ref(),
                std::path::Path::new(&path),
            ));
        }
        tab.bench_scroll(0);
        println!("bench notebook shot {shots}");
    }
    let (code, markdown, outputs, c) = tab.counts();
    println!(
        "bench notebook open cells={} code={code} markdown={markdown} outputs={outputs} \
         pictures={} recoloured={} tables={} latex={}/{} ansi={} placeholders={} ms={ms:.0}",
        tab.cell_count(),
        c.pictures,
        c.recoloured,
        c.tables,
        c.typeset,
        c.latex,
        c.ansi,
        c.placeholders,
    );
}

/// One edit of each kind on `rel`, one undone, then saved: every cell no edit touched is in the
/// file as it was.
async fn edit_drill(app: &Rc<App>, rel: &str) {
    let Some(tab) = app.active_notebook() else {
        return;
    };
    let before = std::fs::read_to_string(app.root().join(rel)).unwrap_or_default();
    let cells = tab.cell_count();
    let source = tab.bench_source(1);
    // Typed a key at a time, one word is one step back and one forward again.
    tab.bench_type(1, "\n#done");
    let typed = tab.bench_source(1) == format!("{source}\n#done");
    tab.undo();
    let typed = typed && tab.bench_source(1) == format!("{source}\n");
    tab.redo();
    let typed = typed && tab.bench_source(1) == format!("{source}\n#done");
    tab.bench_select(1);
    tab.insert(accent_core::notebook::Kind::Markdown, true);
    let inserted = tab.cell_count() == cells + 1;
    tab.bench_select(4);
    let moving = tab.bench_source(4);
    tab.move_cell(false);
    let moved = tab.bench_source(3) == moving;
    tab.bench_select(5);
    tab.delete();
    let deleted = tab.cell_count() == cells;
    tab.undo();
    let undone = tab.cell_count() == cells + 1;
    let dirty = tab.save.modified.get();
    // The arrows run on from one cell into the next, a markdown cell's and a code cell's alike.
    let steps = [
        tab.bench_step(0, true) == Some(1),
        tab.bench_step(1, true) == Some(2),
        tab.bench_step(2, false) == Some(1),
    ];
    println!(
        "bench notebook keys down={} up={}",
        u8::from(steps[0] && steps[1]),
        u8::from(steps[2])
    );
    let saved = app.flush_notebook(&tab).is_ok() && !tab.save.modified.get();
    let after = std::fs::read_to_string(app.root().join(rel)).unwrap_or_default();
    println!(
        "bench notebook edit typed={} inserted={} moved={} deleted={} undone={} dirty={} \
         saved={} untouched_equal={}",
        u8::from(typed),
        u8::from(inserted),
        u8::from(moved),
        u8::from(deleted),
        u8::from(undone),
        u8::from(dirty),
        u8::from(saved),
        // All but the one typed into: a move leaves a cell's bytes as they were.
        u8::from(untouched(&before, &after) == cells - 1),
    );
}

/// A click on a cell's line half under the view's bottom edge leaves the view where it is, and
/// Down from the last line in sight brings the next cell's caret in by the notebook's margin.
async fn scroll_drill(app: &Rc<App>) {
    let Some(tab) = app.active_notebook() else {
        return;
    };
    // Cell 7 built and laid out, the keyboard then in another cell.
    tab.bench_select(7);
    glib::timeout_future(Duration::from_millis(300)).await;
    tab.bench_select(2);
    glib::timeout_future(Duration::from_millis(300)).await;
    let before = tab.bench_click(7);
    // Longer than GtkViewport's scroll to a focused widget takes to play out.
    glib::timeout_future(Duration::from_millis(500)).await;
    let still = before.is_some_and(|b| (tab.bench_scrolled() - b).abs() < 0.5);
    tab.bench_down_at_edge(9);
    glib::timeout_future(Duration::from_millis(300)).await;
    let below = tab.bench_caret_above_edge().unwrap_or(-1.0);
    println!(
        "bench notebook scroll click_still={} down_margin={below:.0}",
        u8::from(still)
    );
}

/// The pane's find bar over the notebook: the readout as it steps, the count against core's over
/// the saved file, a regular expression anchored at each cell's start, a query that does not
/// compile, Replace and Replace All each undone in one step, and Go to Cell.
async fn find_drill(app: &Rc<App>, rel: &str) {
    use crate::find::Mode;
    let Some(tab) = app.active_notebook() else {
        return;
    };
    let text = std::fs::read_to_string(app.root().join(rel)).unwrap_or_default();
    let expected = |query: &str, regex: bool| -> usize {
        let options = accent_core::search::Options {
            regex,
            ..Default::default()
        };
        let re = accent_core::search::pattern(query, options);
        let (Ok(re), Ok(nb)) = (re, accent_core::notebook::read(rel, &text)) else {
            return 0;
        };
        nb.cells()
            .iter()
            .map(|c| re.find_iter(c.source()).count())
            .sum()
    };
    // The query box searches 150 ms after the last change.
    let settle = || glib::timeout_future(Duration::from_millis(300));
    let sources = |tab: &Rc<NotebookTab>| -> Vec<String> {
        (0..tab.cell_count()).map(|n| tab.bench_source(n)).collect()
    };
    let bar = app.pane().find.clone();
    // The caret at the top and the view at the bottom: the first match is scrolled to with the
    // keyboard in the bar.
    tab.goto_cell(0);
    settle().await;
    tab.bench_scroll(10_000);
    settle().await;
    bar.open(Mode::Find);
    bar.query_box().set_text("fidelity");
    settle().await;
    let count = tab.bench_found().0;
    let readout = bar.readout().0;
    let shown = tab.bench_caret_shown();
    bar.step(true);
    let next = bar.readout().0;
    let [_, _, regex] = bar.toggles();
    let sensitive = regex.is_sensitive();
    regex.set_active(true);
    bar.query_box().set_text("^import");
    settle().await;
    let anchored = tab.bench_found().0;
    bar.query_box().set_text("(");
    settle().await;
    let invalid = bar.readout() == ("Invalid pattern".to_string(), true);
    regex.set_active(false);
    bar.query_box().set_text("fidelity");
    settle().await;
    let before = sources(&tab);
    tab.replace("kettle", false);
    let one = tab.bench_found().0 == count - 1;
    tab.undo();
    let one = one && sources(&tab) == before;
    bar.replace_box().set_text("kettle");
    bar.press_replace_all();
    let replaced: usize = sources(&tab)
        .iter()
        .map(|s| s.matches("kettle").count())
        .sum();
    let left = tab.bench_found().0;
    tab.undo();
    let undone = sources(&tab) == before;
    bar.open(Mode::Goto);
    let cells = bar.line_box().placeholder_text().unwrap_or_default();
    bar.line_box().set_text("5");
    bar.line_box().emit_activate();
    let goto = tab.bench_found().1;
    println!(
        "bench notebook find query=fidelity count={count} expected={} readout={readout:?} \
         shown={} next={next:?} regex={anchored}/{} regex_toggle={} invalid={} replace_one={} \
         replace_all={replaced} left={left} one_undo={} goto={cells}:{}",
        expected("fidelity", false),
        u8::from(shown),
        expected("^import", true),
        u8::from(sensitive),
        u8::from(invalid),
        u8::from(one),
        u8::from(undone),
        goto.map_or(-1, |n| n as i64),
    );
}

/// A search hit in each kind of notebook lands on its cell with the match selected: the open
/// `.ipynb`'s and a closed copy's from the index, and the closed marimo notebook's from its file,
/// none with a toast (it used to say a notebook is no text file).
async fn hit_drill(app: &Rc<App>, ipynb: &str, marimo: &str) {
    let copy = "Notebooks/hit.ipynb";
    let toasted = app.toasted.get();
    let _ = std::fs::copy(app.root().join(ipynb), app.root().join(copy));
    super::files::until(|| app.reconciled.get()).await;
    let indexed = |rel: &str| {
        let hits = app.vault()?.search("binomial", 20, false).ok()?;
        hits.into_iter().find(|h| h.rel_path == rel)?.at
    };
    // The copy is the watcher's to index, a moment after the walk.
    for _ in 0..50 {
        if indexed(copy).is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    let text = std::fs::read_to_string(app.root().join(marimo)).unwrap_or_default();
    let marimo_at = text
        .find("best_amplitude")
        .map(|at| at..at + "best_amplitude".len());
    let mut landed = Vec::new();
    for (rel, at) in [
        (ipynb, indexed(ipynb)),
        (copy, indexed(copy)),
        (marimo, marimo_at),
    ] {
        if rel == copy {
            if let Some(sidebar) = app.sidebar.get() {
                sidebar.show_pane("search");
                sidebar.set_search_text("binomial");
                for _ in 0..100 {
                    if sidebar
                        .search_state()
                        .1
                        .iter()
                        .any(|row| row == "hit.ipynb")
                    {
                        break;
                    }
                    glib::timeout_future(Duration::from_millis(20)).await;
                }
                if let (Some(view), Some(row)) = (
                    sidebar.search_view(),
                    sidebar
                        .search_state()
                        .1
                        .iter()
                        .position(|row| row == "hit.ipynb"),
                ) {
                    view.emit_by_name::<()>("activate", &[&(row as u32)]);
                }
            }
        } else if let Some(at) = at {
            app.open_note_at(rel, Some(sidebar::Target::Range(at)));
        }
        let mut tab = None;
        for _ in 0..100 {
            tab = app.doc_for(rel).and_then(|d| d.notebook().cloned());
            if tab.is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let (_, cell, selected) = tab.map(|tab| tab.bench_found()).unwrap_or_default();
        landed.push(format!("{}:{selected}", cell.map_or(-1, |n| n as i64)));
    }
    let quiet = app.toasted.get() == toasted;
    println!(
        "bench notebook hit ipynb={} closed={} marimo={} quiet={}",
        landed[0],
        landed[1],
        landed[2],
        u8::from(quiet)
    );
}

/// The Outline of each notebook, the heading the caret is under, and a row's jump back up to it.
async fn outline_drill(app: &Rc<App>, ipynb: &str, marimo: &str) {
    let Some(sidebar) = app.sidebar.get() else {
        return;
    };
    app.show_pane("outline");
    // The marimo notebook first, so the `.ipynb` is the one in front after.
    let mut first = Vec::new();
    for rel in [marimo, ipynb] {
        if let Some(doc) = app.doc_for(rel) {
            app.reveal_page(doc.page());
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        first.push(
            sidebar
                .outline_lines(None)
                .first()
                .cloned()
                .unwrap_or_default(),
        );
    }
    let Some(tab) = app.active_notebook() else {
        return;
    };
    tab.bench_select(5);
    let (follow, _) = tab.outline_row();
    tab.bench_scroll(10_000);
    glib::timeout_future(Duration::from_millis(300)).await;
    let low = tab.bench_scrolled();
    sidebar.outline_lines(Some(0));
    glib::timeout_future(Duration::from_millis(300)).await;
    let jumped =
        tab.bench_found().1 == Some(0) && tab.bench_scrolled() < low && tab.bench_caret_shown();
    println!(
        "bench notebook outline ipynb={:?} marimo={:?} follow={} jumped={}",
        first[1],
        first[0],
        follow.map_or(-1, |n| n as i64),
        u8::from(jumped)
    );
}

/// How many of `before`'s cells are in `after` byte for byte.
fn untouched(before: &str, after: &str) -> usize {
    use serde_json::value::RawValue;
    let cells = |text| -> Option<Vec<&str>> {
        let doc: std::collections::HashMap<String, &RawValue> = serde_json::from_str(text).ok()?;
        let cells: Vec<&RawValue> = serde_json::from_str(doc.get("cells")?.get()).ok()?;
        Some(cells.into_iter().map(RawValue::get).collect())
    };
    let (Some(before), Some(after)) = (cells(before), cells(after)) else {
        return 0;
    };
    before.iter().filter(|c| after.contains(c)).count()
}

/// The marimo notebook: its cells, and its header written back as it was after an edit.
async fn marimo_drill(app: &Rc<App>, rel: &str) {
    let Some((tab, _)) = opened(app, rel).await else {
        return;
    };
    let before = std::fs::read_to_string(app.root().join(rel)).unwrap_or_default();
    let (_, markdown, _, _) = tab.counts();
    tab.bench_type(3, "\n");
    let _ = app.flush_notebook(&tab);
    let after = std::fs::read_to_string(app.root().join(rel)).unwrap_or_default();
    let header = before.split("\n\n\n").next().unwrap_or_default();
    println!(
        "bench notebook marimo cells={} markdown={markdown} header_kept={}",
        tab.cell_count(),
        u8::from(!header.is_empty() && after.starts_with(header) && after != before),
    );
}

/// The long notebook's open time, and what the process holds once it is open.
async fn big_drill(app: &Rc<App>, rel: &str) {
    let before = rss_mb();
    let t = Instant::now();
    let Some((tab, ms)) = opened(app, rel).await else {
        return;
    };
    // Laid out and drawn: the end of the first frame painted after the tab is there.
    let drawn = Rc::new(Cell::new(None));
    if let Some(clock) = app.window.frame_clock() {
        let (seen, handler) = (drawn.clone(), Rc::new(Cell::new(None)));
        let id = clock.connect_after_paint({
            let handler = handler.clone();
            move |clock| {
                if let Some(id) = handler.take() {
                    clock.disconnect(id);
                }
                seen.set(Some(ms_since(t)));
            }
        });
        handler.set(Some(id));
        clock.request_phase(gdk::FrameClockPhase::PAINT);
    }
    for _ in 0..400 {
        if drawn.get().is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(5)).await;
    }
    glib::timeout_future(Duration::from_millis(500)).await;
    let after = rss_mb();
    let built = tab.bench_built();
    // Scrolled to the end and back up half a screen at a time, the cell at the top of the view
    // moves with the scroll and no further, as the editors above it are built on the way.
    tab.bench_scroll(10_000);
    glib::timeout_future(Duration::from_millis(300)).await;
    let mut drift: f64 = 0.0;
    for _ in 0..12 {
        let Some((n, y)) = tab.bench_top_cell() else {
            break;
        };
        let up = tab.bench_scroll_by(-0.5);
        glib::timeout_future(Duration::from_millis(150)).await;
        if let Some(moved) = tab.bench_cell_y(n) {
            drift = drift.max((moved - y - up).abs());
        }
    }
    println!(
        "bench notebook big cells={} built={built} open_ms={ms:.0} drawn_ms={:.0} rss_mb={after} \
         grew_mb={}",
        tab.cell_count(),
        drawn.get().unwrap_or_default(),
        after.saturating_sub(before),
    );
    println!(
        "bench notebook anchor built={} drift_px={drift:.0}",
        tab.bench_built() - built
    );
}

/// The process's resident size in MB.
fn rss_mb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        })
        .unwrap_or(0)
        / 1024
}

/// A notebook that does not read as one opens as text, saying why; Open as Text shows any
/// notebook's file.
async fn text_drill(app: &Rc<App>, ipynb: &str) {
    let broken = "Notebooks/broken.ipynb";
    let _ = std::fs::write(app.root().join(broken), "{\"cells\": [");
    glib::timeout_future(Duration::from_millis(1500)).await;
    app.open_path(broken);
    let mut unread = false;
    for _ in 0..100 {
        glib::timeout_future(Duration::from_millis(20)).await;
        if let Some(tab) = app.doc_for(broken).and_then(|d| d.tab().cloned()) {
            unread = tab.alert() == Some(crate::editor::Alert::Unread);
            break;
        }
    }
    if let Some(doc) = app.doc_for(ipynb) {
        app.reveal_page(doc.page());
    }
    let _ = WidgetExt::activate_action(&app.window, "win.notebook-open-text", None);
    let mut as_text = false;
    for _ in 0..100 {
        glib::timeout_future(Duration::from_millis(20)).await;
        if app.doc_for(ipynb).is_some_and(|d| d.tab().is_some()) {
            as_text = true;
            break;
        }
    }
    let at = app.vault().and_then(|vault| {
        vault
            .search("binomial", 20, false)
            .ok()?
            .into_iter()
            .find(|hit| hit.rel_path == ipynb)?
            .at
    });
    if let Some(at) = at {
        app.open_note_at(ipynb, Some(sidebar::Target::Range(at)));
    }
    let selected = app
        .doc_for(ipynb)
        .and_then(|doc| doc.tab().cloned())
        .and_then(|tab| {
            let (start, end) = tab.buffer.selection_bounds()?;
            Some(tab.buffer.text(&start, &end, false) == "binomial")
        })
        .unwrap_or(false);
    println!(
        "bench notebook text unread={} open_as_text={} json_hit={}",
        u8::from(unread),
        u8::from(as_text),
        u8::from(selected)
    );
}

async fn present_drill(app: &Rc<App>) {
    let Some(tab) = app.active_notebook() else {
        return;
    };
    app.set_presenting(true);
    glib::timeout_future(Duration::from_millis(300)).await;
    let read_only = tab.bench_read_only();
    app.set_presenting(false);
    println!("bench notebook present read_only={}", u8::from(read_only));
}
