//! `ACCENT_BENCH_NOTEBOOK=<ipynb>,<marimo .py>,<long ipynb>`: the notebook tab on the generated
//! vault's `Notebooks/` (a copy under /tmp: it saves). It prints how the first notebook's outputs
//! show, an edit of each kind with its undo and the save that follows, the marimo notebook kept
//! as written, the long notebook's open time and memory, and a presented notebook's read-only.
//! All under Dark, which recolours the pictures. `ACCENT_BENCH_NOTEBOOK_SHOT=<png>` adds a picture
//! of the window with the first notebook open, for looking at rather than asserting on.

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
    println!(
        "bench notebook big cells={} open_ms={ms:.0} drawn_ms={:.0} rss_mb={after} grew_mb={}",
        tab.cell_count(),
        drawn.get().unwrap_or_default(),
        after.saturating_sub(before),
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
    println!(
        "bench notebook text unread={} open_as_text={}",
        u8::from(unread),
        u8::from(as_text)
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
