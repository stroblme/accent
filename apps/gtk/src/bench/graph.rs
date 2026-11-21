//! The Graph tab: what it shows around the file in front, where a press on a node opens that
//! file and where the keyboard is left, and the tab coming back with the session.

use super::*;
use crate::graph::GraphTab;

/// `ACCENT_BENCH_GRAPH=check:<rel>` opens `rel`, then Show Graph, and prints the nodes, a ghost's
/// key ending in `?`, and the links once laid out (`open`). It presses two of them in turn, each
/// opening beside the graph as the other pane's preview, and prints where each opened and where the
/// keyboard is (`click`), and the roots and nodes the graph grew to (`grow`). It writes a link to
/// `d.md` into `c.md` and prints the node that adds and whether every node kept its index
/// (`update`), then renames `b.md` and prints whether its node kept its index and the ghost the
/// link to its old name leaves (`rename`). Through the menu's commands it shows tags and images and
/// prints the nodes each adds (`tags`, `images`); a press on the ghost prints the name New File
/// offers (`ghost`); it shows the whole vault without its orphans and then with them (`all`,
/// `orphans`), and prints Reset Graph's roots (`reset`). It types `c.md` over the canvas with real
/// keys (XTEST), the first of which opens the pane's find bar, and prints what the bar's query
/// holds, the node the keyboard went to and the bar's readout (`find`), and where the keyboard is
/// once the bar is closed (`find closed`); it presses the four arrows and prints whether the
/// keyboard moved (`arrows`), then Enter and prints whether the node it is on opened (`enter`).
/// Then it saves the session and quits. `restored` prints whether the graph came back with the
/// session, the file it is around (`session`) and what its menu shows (`saved`); opens `lone.md`,
/// which it does not hold, and with animations off shows the graph again, printing what it started
/// again around and whether it showed no places between where the nodes were placed and the settled
/// layout (`still`); then closes it and prints whether the layout's threads ended with it
/// (`closed`). `drill_graph` writes the notes into a scratch vault. `time` is described at
/// [`time`].
pub(super) fn bench_graph(app: &Rc<App>, arg: &str) {
    let app = app.clone();
    let arg = arg.to_string();
    glib::spawn_future_local(async move {
        until("graph", "reconciled", 60_000, || app.reconciled.get()).await;
        match arg.split_once(':') {
            Some(("check", rel)) => check(&app, rel).await,
            None if arg == "restored" => restored(&app).await,
            None if arg == "time" => time(&app).await,
            _ => eprintln!("ACCENT_BENCH_GRAPH=check:<rel>, restored or time"),
        }
        bench_quit(&app);
    });
}

async fn check(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    if until("graph", "opened", 10_000, || app.tab_for(rel).is_some())
        .await
        .is_none()
    {
        return;
    }
    let _ = WidgetExt::activate_action(&app.window, "win.graph", None);
    let Some(graph) = laid_out(app).await else {
        return;
    };
    println!(
        "bench graph open front={} nodes={:?} links={}",
        graph.front().unwrap_or_default(),
        graph.nodes(),
        graph.links()
    );
    for key in ["c.md", "b.md"] {
        graph.press(key, false);
        if until("graph", key, 10_000, || app.tab_for(key).is_some())
            .await
            .is_none()
        {
            return;
        }
        // The keyboard is handed back from an idle.
        glib::timeout_future(Duration::from_millis(200)).await;
        let page = app.tab_for(key).map(|tab| tab.page.clone());
        let pane = page.as_ref().and_then(|page| app.pane_of(page));
        let n = (pane.as_ref())
            .and_then(|pane| app.panes.borrow().iter().position(|p| Rc::ptr_eq(p, pane)))
            .map_or(0, |n| n + 1);
        let preview = pane.as_ref().and_then(|pane| pane.preview()) == page;
        let keyboard = keyboard(app, &graph);
        let opened = (pane.as_ref())
            .and_then(|pane| app.doc_of(pane))
            .map(|doc| doc.key());
        let tabs = pane.map_or(0, |pane| pane.tabs.n_pages());
        println!(
            "bench graph click {key} opened={} pane={n} preview={preview} keyboard={keyboard} \
             tabs={tabs}",
            opened.unwrap_or_default()
        );
        until("graph", "grown", 10_000, || graph.landed()).await;
        println!(
            "bench graph grow {key} roots={:?} nodes={:?} front={}",
            graph.roots(),
            graph.nodes(),
            graph.front().unwrap_or_default()
        );
    }
    let root = app.root();
    let before: Vec<(String, Option<usize>)> = (graph.nodes().into_iter())
        .map(|key| (key.clone(), graph.index_of(&key)))
        .collect();
    if let Err(e) = std::fs::write(root.join("c.md"), "[[f]] [[d]]\n") {
        return println!("bench graph update failed {e}");
    }
    until("graph", "update", 10_000, || {
        graph.index_of("d.md").is_some() && graph.landed()
    })
    .await;
    let added: Vec<String> = (graph.nodes().into_iter())
        .filter(|n| !before.iter().any(|(k, _)| k == n))
        .collect();
    let kept = (before.iter()).all(|(key, at)| graph.index_of(key) == *at);
    println!("bench graph update c.md nodes+={added:?} kept={kept}");
    // `a.md`'s link to `b.md` dangles once it has gone: a ghost in its place.
    let (was, before) = (graph.index_of("b.md"), graph.nodes());
    if let Err(e) = std::fs::rename(root.join("b.md"), root.join("bee.md")) {
        return println!("bench graph rename failed {e}");
    }
    until("graph", "rename", 10_000, || {
        graph.nodes().iter().any(|n| n == "b.md?") && graph.landed()
    })
    .await;
    let added: Vec<String> = (graph.nodes().into_iter())
        .filter(|n| !before.contains(n))
        .collect();
    println!(
        "bench graph rename b.md bee.md index_kept={} roots={:?} nodes+={added:?}",
        was.is_some() && graph.index_of("bee.md") == was,
        graph.roots()
    );
    for (action, what) in [("win.graph-tags", "tags"), ("win.graph-images", "images")] {
        let before = graph.nodes();
        menu(app, &graph, action).await;
        let added: Vec<String> = (graph.nodes().into_iter())
            .filter(|n| !before.contains(n))
            .collect();
        println!("bench graph {what} nodes+={added:?}");
    }
    graph.press("gone.md", false);
    let dialog = until("graph", "new file", 10_000, || app.window.visible_dialog()).await;
    let offered = (dialog.as_ref())
        .and_then(|d| {
            find_widget(d.upcast_ref(), &|w| {
                w.downcast_ref::<gtk::Entry>()
                    .is_some_and(|e| e.placeholder_text().as_deref() == Some("File name"))
            })
        })
        .and_downcast::<gtk::Entry>()
        .map(|entry| entry.text().to_string());
    println!("bench graph ghost gone.md dialog={offered:?}");
    if let Some(dialog) = dialog {
        dialog.force_close();
    }
    menu(app, &graph, "win.graph-all").await;
    println!("bench graph all orphans=false nodes={:?}", graph.nodes());
    let before = graph.nodes();
    menu(app, &graph, "win.graph-orphans").await;
    let added: Vec<String> = (graph.nodes().into_iter())
        .filter(|n| !before.contains(n))
        .collect();
    println!("bench graph orphans nodes+={added:?}");
    menu(app, &graph, "win.graph-reset").await;
    println!(
        "bench graph reset all={} roots={:?} nodes={:?}",
        graph.saved().all,
        graph.roots(),
        graph.nodes()
    );
    let Some(pane) = app.pane_of(&graph.page) else {
        return;
    };
    // Typed with real keys over the canvas: the first key opens the bar and lands in its query.
    let canvas = graph.key_target();
    canvas.grab_focus();
    let at = gtk::graphene::Point::new(canvas.width() as f32 / 2.0, 8.0);
    let at = canvas.compute_point(&app.window, &at).unwrap_or(at);
    let keys = format!(
        "move {:.0} {:.0}; focus; type c; key period; type md",
        at.x(),
        at.y()
    );
    super::git::xtest(&keys).await;
    until("graph", "find", 10_000, || {
        graph.current().as_deref() == Some("c.md")
    })
    .await;
    println!(
        "bench graph find typed={:?} current={} readout={:?}",
        pane.find.query_box().text(),
        graph.current().unwrap_or_default(),
        pane.find.readout().0
    );
    pane.find.close();
    glib::timeout_future(Duration::from_millis(200)).await;
    println!(
        "bench graph find closed keyboard={} current={}",
        keyboard(app, &graph),
        graph.current().unwrap_or_default()
    );
    let from = graph.current();
    let mut moved = false;
    for key in [
        gdk::Key::Right,
        gdk::Key::Down,
        gdk::Key::Left,
        gdk::Key::Up,
    ] {
        graph.press_key(key);
        moved |= graph.current() != from;
    }
    println!("bench graph arrows moved={moved}");
    let current = graph.current().unwrap_or_default();
    graph.press_key(gdk::Key::Return);
    let shown = || {
        (app.panes.borrow().iter())
            .any(|pane| app.doc_of(pane).is_some_and(|doc| doc.key() == current))
    };
    let opened = until("graph", "enter", 10_000, shown).await.is_some();
    println!("bench graph enter opened_current={opened}");
    // Where the arrows ended depends on the layout's shape: the session is left on one file.
    app.open_path("c.md");
    until("graph", "c.md", 10_000, || {
        app.active_key().as_deref() == Some("c.md")
    })
    .await;
    app.save_session();
}

/// Where the keyboard is: on the graph, elsewhere, or nowhere.
fn keyboard(app: &App, graph: &GraphTab) -> &'static str {
    match app.focused() {
        Some(focus) if focus.is_ancestor(&graph.page.child()) => "graph",
        Some(_) => "elsewhere",
        None => "none",
    }
}

/// Run the graph's menu command `action` and wait for its answer to be laid out.
async fn menu(app: &Rc<App>, graph: &GraphTab, action: &str) {
    let _ = WidgetExt::activate_action(&app.window, action, None);
    until("graph", action, 10_000, || graph.landed()).await;
}

async fn restored(app: &Rc<App>) {
    let Some(graph) = laid_out(app).await else {
        return;
    };
    println!(
        "bench graph session restored=true front={}",
        graph.front().unwrap_or_default()
    );
    let saved = graph.saved();
    println!(
        "bench graph saved all={} unresolved={} tags={} images={} orphans={}",
        saved.all,
        saved.shown.unresolved,
        saved.shown.tags,
        saved.shown.images,
        saved.shown.orphans
    );
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(false);
    }
    app.open_path("lone.md");
    until("graph", "lone.md", 10_000, || {
        app.tab_for("lone.md").is_some()
    })
    .await;
    let _ = WidgetExt::activate_action(&app.window, "win.graph", None);
    until("graph", "still", 10_000, || {
        graph.roots() == ["lone.md"] && graph.laid_out()
    })
    .await;
    println!(
        "bench graph still roots={:?} nodes={:?} settled_at_once={}",
        graph.roots(),
        graph.nodes(),
        graph.frames() <= 2
    );
    let life = graph.worker_life();
    app.close_page(&graph.page);
    until("graph", "closed", 10_000, || app.graphs().is_empty()).await;
    drop(graph);
    let ended = until("graph", "worker", 10_000, || {
        life.as_ref().is_none_or(|life| life.upgrade().is_none())
    })
    .await
    .is_some();
    println!("bench graph closed worker_ended={ended}");
}

/// The window's graph once it shows a laid-out answer.
async fn laid_out(app: &Rc<App>) -> Option<Rc<GraphTab>> {
    let graph = until("graph", "graph", 10_000, || app.graphs().into_iter().next()).await?;
    until("graph", "laid out", 30_000, || graph.laid_out()).await?;
    Some(graph)
}

/// A case's budget (plan, 2026-11-21, the whole vault's tightened from its first run): the first
/// paint and a frame's paint, the worst gap in the main loop, and how many of the layout's frames
/// it settles in, two at most with animations off (where the nodes were placed, then settled).
struct Budget {
    first_ms: Option<f64>,
    paint_ms: Option<f64>,
    worst_ms: f64,
    frames: Frames,
}

enum Frames {
    AtLeast(usize),
    AtMost(usize),
    Any,
}

/// `ACCENT_BENCH_GRAPH=time`, on the generated vault: the graph round its best-linked note, grown
/// by the neighbour that adds the most (as a click grows it, without the file's open), the whole
/// vault, then the local graph and the whole vault again with animations off, and [`input`] while
/// the local graph and the whole vault build. First it prints what the vault's two answers cost on
/// their worker (`vault`). Each case prints its nodes and links, the first paint from the command
/// (the first picture of what it shows on screen) and the part of it until the answer landed
/// (`query_ms`), the settle, the layout's frames, the median and the longest paint, each window
/// frame (`frame_ms`, frame clock to frame clock), the worker's draws, the answer's landing, the
/// main loop's worst gap and busy time (`native::Beat`), the main thread's CPU time and its wait
/// for a CPU (`native::schedstat`), the RSS, and whether that kept to the case's [`Budget`].
///
/// On AMCOMP (2026-11-21, Xvfb, cairo, `nice -n 19`, load 5–7, three runs): the vault answers
/// round the note in 30–61 ms and with the whole vault in 109–233 ms. Round the note (668 nodes,
/// 1889 links) the first paint takes 103–141 ms (query 56–91), the layout settles in 5.1 s over
/// 234–273 frames, a frame takes 17 ms median and 47 at most, a third of it the names, a draw
/// 16 ms, and the worst gap is 45–51 ms. Grown (926, 3457) a draw takes 25 ms and the worst gap
/// is 116–125 ms, between frames, where the opened note's text view highlights in idle callbacks
/// (NOTEPAD, PERF). The whole vault (7711, 48889) first paints in 300–334 ms, settles in 5.6 s,
/// draws in 110 ms (237 at most), its frames 0 ms median and 38 at most, worst gap 43–45 ms. With
/// animations off round the note it paints in 64–66 ms and settles in 0.3 s, worst gap 25–32 ms,
/// and the whole vault in 201–242 ms and 4.3 s, worst gap 44 ms. An input while the note's graph
/// settles shows in 16–19 ms (37 at most) and its picture in 69–82 ms; while the whole vault
/// builds in 19–28 ms (47) and 236–257 ms (414). RSS stays at 256–335 MB. GL on llvmpipe is no
/// better here: 21 ms frames round the note. So round the note the first paint and the worst gap
/// are over budget, the names costing cairo a frame's budget on their own, and the whole vault
/// keeps to its own. Drawn with cairo under Xvfb, so a paint costs more than a GL renderer's.
async fn time(app: &Rc<App>) {
    let Some(vault) = app.vault().cloned() else {
        return println!("bench graph time no_vault");
    };
    let shown = accent_core::graph::Shown::default();
    let part = work::off_thread("graph", move || vault.graph_all(shown)).await;
    let Some(Ok(part)) = part else {
        return println!("bench graph time no_graph");
    };
    let mut degree = vec![0; part.nodes.len()];
    for e in &part.edges {
        degree[e.from as usize] += 1;
        degree[e.to as usize] += 1;
    }
    let note = accent_core::graph::NodeKind::File(Some(accent_core::query::Doc::Note));
    let best = (part.nodes.iter().enumerate())
        .filter(|(_, n)| n.kind == note)
        .max_by_key(|(i, _)| degree[*i])
        .map(|(_, n)| n.key.clone());
    let Some(best) = best else {
        return println!("bench graph time no_note");
    };
    // The neighbour whose own neighbours add the most to the graph round `best`.
    let mut near = vec![Vec::new(); part.nodes.len()];
    for e in &part.edges {
        near[e.from as usize].push(e.to as usize);
        near[e.to as usize].push(e.from as usize);
    }
    let at = part.nodes.iter().position(|n| n.key == best).unwrap_or(0);
    let shown: std::collections::HashSet<usize> = near[at].iter().copied().chain([at]).collect();
    let adds = |j: usize| near[j].iter().filter(|k| !shown.contains(k)).count();
    let grow_by = (near[at].iter().copied())
        .filter(|&j| part.nodes[j].kind == note)
        .max_by_key(|&j| adds(j))
        .map(|j| part.nodes[j].key.clone());
    // What the vault's answers cost on their worker, which each first paint waits for: the
    // fastest of three.
    let (Some(vault), roots) = (app.vault().cloned(), vec![best.clone()]) else {
        return;
    };
    let shown = accent_core::graph::Shown::default();
    let timed = work::off_thread("graph", move || {
        let fastest = |f: &dyn Fn()| {
            (0..3)
                .map(|_| {
                    let t = Instant::now();
                    f();
                    ms_since(t)
                })
                .fold(f64::MAX, f64::min)
        };
        let around = fastest(&|| {
            let _ = vault.graph_around(&roots, shown);
        });
        let all = fastest(&|| {
            let _ = vault.graph_all(shown);
        });
        (around, all)
    })
    .await;
    if let Some((around, all)) = timed {
        println!("bench graph vault around_ms={around:.0} all_ms={all:.0}");
    }
    app.open_path(&best);
    until("graph", "note", 30_000, || app.tab_for(&best).is_some()).await;
    // The note's text view lays out and highlights in idle callbacks at first, holding the loop
    // up to ~190 ms (NOTEPAD, PERF): the graph is timed once that is done.
    glib::timeout_future(Duration::from_secs(2)).await;
    let budget = |first_ms, paint_ms, worst_ms, frames| Budget {
        first_ms,
        paint_ms,
        worst_ms,
        frames,
    };
    let local = budget(Some(100.0), Some(4.0), 16.0, Frames::AtLeast(30));
    measure(app, "local", local, || {
        let _ = WidgetExt::activate_action(&app.window, "win.graph", None);
    })
    .await;
    let Some(graph) = app.graphs().into_iter().next() else {
        return;
    };
    let grow = budget(None, Some(4.0), 16.0, Frames::AtLeast(15));
    if let Some(next) = grow_by {
        measure(app, "grow", grow, || app.bench_grow(&graph, &next)).await;
    }
    input(app, "settling", || command(app, "win.graph-reset")).await;
    let all = budget(Some(500.0), Some(4.0), 50.0, Frames::Any);
    measure(app, "all", all, || command(app, "win.graph-all")).await;
    input(app, "vault", || {
        command(app, "win.graph-reset");
        command(app, "win.graph-all");
    })
    .await;
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(false);
    }
    let still = budget(Some(100.0), None, 16.0, Frames::AtMost(2));
    measure(app, "local_still", still, || {
        command(app, "win.graph-reset")
    })
    .await;
    let still = budget(Some(500.0), None, 50.0, Frames::AtMost(2));
    measure(app, "all_still", still, || command(app, "win.graph-all")).await;
}

/// `input`: `act`, a cold layout round the file in front or the whole vault, then a pan, a zoom
/// or the pointer over a node every 50 ms while it settles and draws, 30 in all. Prints the main
/// loop's worst gap meanwhile, how long each input took to show (`shown_ms`, the next frame,
/// with the last picture moved and scaled), and how long until the worker's picture of it was
/// on screen (`drawn_ms`).
async fn input(app: &Rc<App>, phase: &str, act: impl FnOnce()) {
    let Some(graph) = app.graphs().into_iter().next() else {
        return;
    };
    let Some(clock) = graph.frame_clock() else {
        return;
    };
    let frames = Rc::new(RefCell::new(Vec::<(Instant, u64, u64)>::new()));
    let hook = clock.connect_after_paint({
        let (frames, graph) = (frames.clone(), graph.clone());
        move |_| {
            let (asked, shown) = graph.serials();
            frames.borrow_mut().push((Instant::now(), asked, shown));
        }
    });
    let beat = super::native::Beat::start();
    act();
    let mut inputs = Vec::new();
    for k in 0..30 {
        glib::timeout_future(Duration::from_millis(50)).await;
        inputs.push(Instant::now());
        graph.bench_input(k);
    }
    glib::timeout_future(Duration::from_millis(500)).await;
    let (worst, _) = beat.stop();
    clock.disconnect(hook);
    let frames = frames.take();
    let (mut shown, mut drawn) = (Vec::new(), Vec::new());
    for &t in &inputs {
        let Some(&(at, asked, _)) = frames.iter().find(|f| f.0 > t) else {
            continue;
        };
        shown.push((at - t).as_secs_f64() * 1e3);
        if let Some(&(done, ..)) = frames.iter().find(|f| f.0 >= at && f.2 >= asked) {
            drawn.push((done - t).as_secs_f64() * 1e3);
        }
    }
    let sorted = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let median = v.get(v.len() / 2).copied().unwrap_or(-1.0);
        (median, v.last().copied().unwrap_or(-1.0), v.len())
    };
    let ((s_median, s_max, s_n), (d_median, d_max, d_n)) = (sorted(shown), sorted(drawn));
    println!(
        "bench graph input {phase} inputs={} worst_ms={worst:.0} shown_ms_median={s_median:.0} \
         shown_ms_max={s_max:.0} shown={s_n} drawn_ms_median={d_median:.0} drawn_ms_max={d_max:.0} \
         drawn={d_n} nodes={}",
        inputs.len(),
        graph.node_count()
    );
}

fn command(app: &App, action: &str) {
    let _ = WidgetExt::activate_action(&app.window, action, None);
}

/// One case: `act` run, then the graph's answer waited for until it is laid out, and timed. Each
/// of the window's frames is timed too, from its frame clock's before-paint to its after-paint
/// (the layout's frame taken, layout, snapshot and render), the first three of the whole vault's
/// printed as they come, since one can take long.
async fn measure(app: &Rc<App>, case: &str, budget: Budget, act: impl FnOnce()) {
    if let Some(graph) = app.graphs().first() {
        graph.take_paints();
        graph.take_draws();
        graph.take_landing_ms();
    }
    println!("bench graph {case} start");
    let beat = super::native::Beat::start();
    let cpu = super::native::schedstat();
    let t = Instant::now();
    act();
    // The command itself, on the main loop: a new tab and the window's syncs for the first.
    let act_ms = ms_since(t);
    let frame_ms = Rc::new(RefCell::new(Vec::new()));
    let mut hooked = None;
    let mut first = None;
    let settled = loop {
        let graph = app.graphs().into_iter().next();
        if let Some(graph) = &graph {
            if hooked.is_none()
                && let Some(clock) = graph.frame_clock()
            {
                let start = Rc::new(Cell::new(None::<Instant>));
                let before = clock.connect_before_paint({
                    let start = start.clone();
                    move |_| start.set(Some(Instant::now()))
                });
                let (frames, case) = (frame_ms.clone(), case.to_string());
                let after = clock.connect_after_paint(move |_| {
                    if let Some(t) = start.take() {
                        let mut frames = frames.borrow_mut();
                        frames.push(ms_since(t));
                        if case == "all" && frames.len() <= 3 {
                            println!(
                                "bench graph all frame={} frame_ms={:.0}",
                                frames.len(),
                                ms_since(t)
                            );
                        }
                    }
                });
                hooked = Some((clock, before, after));
            }
            if first.is_none()
                && let Some(at) = graph.shown_at().filter(|&at| at > t)
            {
                first = Some((at - t).as_secs_f64() * 1e3);
            }
            if graph.laid_out() && first.is_some() {
                break Some(ms_since(t));
            }
        }
        if t.elapsed() > Duration::from_secs(120) {
            break None;
        }
        glib::timeout_future(Duration::from_millis(1)).await;
    };
    let (worst, busy) = beat.stop();
    let used = super::native::schedstat();
    let (cpu, wait) = (used.0 - cpu.0, used.1 - cpu.1);
    if let Some((clock, before, after)) = hooked {
        clock.disconnect(before);
        clock.disconnect(after);
    }
    let Some(graph) = app.graphs().into_iter().next() else {
        return;
    };
    let sorted = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let median = v.get(v.len() / 2).copied().unwrap_or(0.0);
        (median, v.last().copied().unwrap_or(0.0))
    };
    let (median, longest) = sorted(graph.take_paints());
    let (frame_median, frame_max) = sorted(frame_ms.take());
    let (draw_median, draw_max) = sorted(graph.take_draws());
    // The first paint split into the vault's answer landing and the rest, its first picture.
    let query_ms = graph
        .landed_at()
        .map_or(-1.0, |at| (at - t).as_secs_f64() * 1000.0);
    let landing_ms = graph.take_landing_ms();
    let frames = graph.frames();
    let ok = settled.is_some()
        && budget
            .first_ms
            .is_none_or(|ms| first.is_some_and(|f| f <= ms))
        && budget.paint_ms.is_none_or(|ms| median <= ms)
        && worst <= budget.worst_ms
        && match budget.frames {
            Frames::AtLeast(n) => frames >= n,
            Frames::AtMost(n) => frames <= n,
            Frames::Any => true,
        };
    println!(
        "bench graph {case} nodes={} links={} first_paint_ms={:.0} query_ms={query_ms:.0} \
         settle_ms={:.0} frames={frames} \
         paint_ms_median={median:.1} paint_ms_max={longest:.1} frame_ms_median={frame_median:.0} \
         frame_ms_max={frame_max:.0} draw_ms_median={draw_median:.1} draw_ms_max={draw_max:.1} \
         act_ms={act_ms:.0} landing_ms={landing_ms:.0} worst_ms={worst:.0} busy_ms={busy:.0} \
         cpu_ms={cpu:.0} wait_ms={wait:.0} rss_mb={} budget={}",
        graph.node_count(),
        graph.links(),
        first.unwrap_or(-1.0),
        settled.unwrap_or(-1.0),
        super::notebook::rss_mb(),
        if ok { "ok" } else { "over" }
    );
}
