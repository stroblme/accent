//! The Graph tab: what it shows around the file in front, where a press on a node opens that
//! file and where the keyboard is left, and the tab coming back with the session.

use super::*;
use crate::graph::GraphTab;

/// `ACCENT_BENCH_GRAPH=check:<rel>` opens `rel`, then Show Graph, and prints the nodes, a ghost's
/// key ending in `?`, and the links once laid out (`open`). It clicks `b.md` with the pointer,
/// which splits the window, and narrows the canvas from under it ([`split`]), then presses two
/// nodes in turn, each opening beside the graph as the other pane's preview, and prints where each
/// opened and where the keyboard is (`click`), and the roots and nodes the graph grew to (`grow`).
/// It writes a link to `d.md` into `c.md` and prints the node that adds and whether every node kept
/// its index (`update`), then renames `b.md` and prints whether its node kept its index and the
/// ghost the link to its old name leaves (`rename`). Through the menu's commands it shows tags and
/// images and prints the nodes each adds (`tags`, `images`), then toggles tags four times in a row
/// and prints how many questions went to the vault and whether the graph ended as it began
/// (`toggles`); a press on the ghost prints the name New File offers (`ghost`); it shows the whole
/// vault without its orphans and then with them (`all`, `orphans`), and prints Reset Graph's roots
/// (`reset`). Behind another tab of its pane it has `e.md` link its root and prints how many
/// questions went while it was hidden and once it was shown again, and the node that added
/// (`hidden`). It types `c.md` over the canvas with real keys (XTEST), the first of which opens the
/// pane's find bar, and prints what the bar's query holds, the node the keyboard went to and the
/// bar's readout (`find`), and where the keyboard is once the bar is closed (`find closed`); it
/// presses the four arrows and prints whether the keyboard moved (`arrows`), then Enter on `a.md`
/// and prints whether it opened (`enter`).
/// Then it leaves the keyboard on `a.md`, the graph alone in its pane, saves the session and quits.
/// `restored` prints whether the graph came back with the session, the file it is around
/// (`session`) and what its menu shows (`saved`); opens `lone.md`, which it does not hold, and with
/// animations off shows the graph again, printing what it started again around and whether it
/// showed no places between where the nodes were placed and the settled layout (`still`); then
/// closes it and prints whether the layout's threads ended with it (`closed`). `drill_graph` writes
/// the notes into a scratch vault. `time` is described at [`time`].
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
    split(app, &graph).await;
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
    // Four toggles in a row, faster than an answer comes: one question goes, the newest of the
    // other three waits for it, and the graph ends as it began.
    let (before, asked) = (graph.nodes(), graph.questions());
    for _ in 0..4 {
        command(app, "win.graph-tags");
    }
    until("graph", "toggles", 10_000, || graph.landed()).await;
    println!(
        "bench graph toggles queries={} same={}",
        graph.questions() - asked,
        graph.nodes() == before
    );
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
    // Behind another tab of its pane the graph asks nothing as the vault changes, and once shown
    // again it asks once: `e.md` now links `bee.md`, the root.
    let behind = (app.tab_for("a.md").map(|tab| tab.page.clone()))
        .filter(|page| app.pane_of(page).is_some_and(|p| Rc::ptr_eq(&p, &pane)));
    let Some(behind) = behind else {
        return println!("bench graph hidden no_tab_behind");
    };
    pane.tabs.set_selected_page(&behind);
    let (asked, before) = (graph.questions(), graph.nodes());
    if let Err(e) = std::fs::write(root.join("e.md"), "[[a]] [[bee]]\n") {
        return println!("bench graph hidden failed {e}");
    }
    // Past the watcher's delay and the graph's own.
    glib::timeout_future(Duration::from_millis(1500)).await;
    let hidden = graph.questions() - asked;
    pane.tabs.set_selected_page(&graph.page);
    until("graph", "shown again", 10_000, || {
        graph.index_of("e.md").is_some() && graph.landed()
    })
    .await;
    let added: Vec<String> = (graph.nodes().into_iter())
        .filter(|n| !before.contains(n))
        .collect();
    println!(
        "bench graph hidden queries={hidden} shown_again={} nodes+={added:?}",
        graph.questions() - asked - hidden
    );
    // Typed with real keys over the canvas, 30 ms apart: the first key opens the bar and lands in
    // its query, and the next comes while the bar is still sliding in.
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
    // Enter on `e.md`, which adds `a.md` beside it, then on `a.md`, which moves it from behind the
    // graph to the other pane and leaves the graph alone in its own: restored, it waits there for
    // the files to be back before it starts round the one in front.
    for key in ["e.md", "a.md"] {
        graph.find(key, accent_core::search::Options::default());
        graph.find_clear();
        graph.press_key(gdk::Key::Return);
        let shown = || {
            (app.panes.borrow().iter())
                .any(|pane| app.doc_of(pane).is_some_and(|doc| doc.key() == key))
        };
        until("graph", key, 10_000, || shown() && graph.landed()).await;
    }
    let pane = app.tab_for("a.md").and_then(|tab| app.pane_of(&tab.page));
    let alone = app
        .pane_of(&graph.page)
        .is_some_and(|p| p.tabs.n_pages() == 1);
    println!(
        "bench graph enter a.md moved={} alone={alone}",
        pane.as_ref()
            .is_some_and(|p| app.pane_of(&graph.page).is_none_or(|g| !Rc::ptr_eq(p, &g)))
    );
    // The session is left on `a.md`.
    if let Some(pane) = pane {
        app.focus_document(&pane);
    }
    until("graph", "a.md", 10_000, || {
        app.active_key().as_deref() == Some("a.md")
    })
    .await;
    app.save_session();
}

/// A real click on `b.md` in a window of one pane, which splits it, on a graph the reader moved,
/// with the node where the narrower canvas leaves it off: once the split is laid out the canvas
/// has eased over to the node, the file in front now (`split`). Then, the pointer resting on the
/// node at the canvas's right edge, the divider moves left from under it, which GTK tells the
/// canvas nothing of: once laid out the node is lit no more (`narrowed`).
async fn split(app: &Rc<App>, graph: &GraphTab) {
    let canvas = graph.key_target();
    let (w, h) = (f64::from(canvas.width()), f64::from(canvas.height()));
    let point = |x: f64, y: f64| {
        let at = gtk::graphene::Point::new(x as f32, y as f32);
        let at = canvas.compute_point(&app.window, &at).unwrap_or(at);
        let (dx, dy) = app.window.surface_transform();
        format!(
            "{:.0} {:.0}",
            f64::from(at.x()) + dx,
            f64::from(at.y()) + dy
        )
    };
    graph.put("b.md", [0.8 * w, 0.5 * h]);
    let at = point(0.8 * w, 0.5 * h);
    super::git::xtest_now(&format!("move {at}; down; up")).await;
    until("graph", "b.md", 10_000, || app.tab_for("b.md").is_some()).await;
    until("graph", "split", 5_000, || {
        graph.front_shown() && !graph.easing()
    })
    .await;
    println!(
        "bench graph split b.md front={} shown={} panes={}",
        graph.front().unwrap_or_default(),
        graph.front_shown(),
        app.panes.borrow().len()
    );
    let paned = graph.page.child().ancestor(gtk::Paned::static_type());
    let Some(paned) = paned.and_downcast::<gtk::Paned>() else {
        return println!("bench graph narrowed no_split");
    };
    let (w, h) = (f64::from(canvas.width()), f64::from(canvas.height()));
    graph.put("b.md", [w - 40.0, 0.5 * h]);
    super::git::xtest_now(&format!("move {}", point(w - 40.0, 0.5 * h))).await;
    let lit = until("graph", "lit", 5_000, || graph.lit()).await;
    paned.set_position(paned.position() - 120);
    until("graph", "narrowed", 5_000, || {
        graph.lit().is_none() && f64::from(canvas.width()) < w
    })
    .await;
    println!(
        "bench graph narrowed lit_before={} lit={}",
        lit.as_deref().unwrap_or("none"),
        graph.lit().as_deref().unwrap_or("none")
    );
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
/// paint and a window frame's median, the worst gap in the main loop, and how many of the
/// layout's frames it settles in, two at most with animations off (where the nodes were placed,
/// then settled).
struct Budget {
    first_ms: Option<f64>,
    frame_ms: Option<f64>,
    worst_ms: f64,
    frames: Frames,
}

enum Frames {
    AtLeast(usize),
    AtMost(usize),
    Any,
}

/// `v`'s median, 90th percentile and longest, as `median/p90/max`; `-` for none.
fn spread(mut v: Vec<f64>) -> String {
    v.sort_by(f64::total_cmp);
    match v.len() {
        0 => "-".to_string(),
        n => format!(
            "{:.1}/{:.1}/{:.1}",
            v[n / 2],
            v[(n * 9 / 10).min(n - 1)],
            v[n - 1]
        ),
    }
}

/// `v`'s median, or `-1` for none.
fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(-1.0)
}

/// What tells two answers apart: a hash of all of `part`, its order included.
fn digest(part: &accent_core::graph::Part) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{part:?}").hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// `ACCENT_BENCH_GRAPH=time`, on the generated vault: the graph round its best-linked note, grown
/// by the neighbour that adds the most (as a click grows it, without the file's open), the whole
/// vault, then its tags shown and hidden again (300 tag nodes and their edges added, then removed),
/// then the local graph and the whole vault again with animations off, [`input`] while the local
/// graph and the whole vault build, and [`hover`] over each once settled. Run it again with
/// `GDK_SCALE=2` for a 2× display, which every line names as `scale`.
///
/// First it prints what the vault's three answers cost on their worker and a digest of each, to
/// tell two builds' answers apart (`vault`), and [`concurrent`]. Each case prints its nodes before
/// (`was`) and after, links, the first paint from the command (the first window frame presented
/// after a picture of what it shows went into a snapshot, `first_paint_ms`, and that snapshot,
/// `first_snapshot_ms`) and the part of it until the answer landed (`query_ms`), the settle, the
/// layout's frames, the canvas's snapshots (`snapshot_ms`), each window frame (`frame_ms`, its
/// frame clock's before-paint to after-paint: the layout's frame taken, layout, snapshot and
/// render), the worker's draws, the answer's landing on the main loop and the diff part of it, the
/// questions that went to the vault, the main loop's worst gap and busy time (`native::Beat`), the
/// main thread's CPU time and its wait for a CPU (`native::schedstat`), the RSS, and whether that
/// kept to the case's [`Budget`]. A spread is `median/p90/max`.
///
/// On AMCOMP (2026-11-22, main ede76acc, Xvfb, cairo, `nice -n 19`, load 6–10, five runs, the
/// medians): the vault answers round the note in 11 ms, grown in 16 and with the whole vault in
/// 113, and a search, the file tree and backlinks wait for none of them (54, 0.2 and 0.3 ms
/// meanwhile). Round the note (668 nodes, 1889 links) the first frame shows it 101 ms after Show
/// Graph (query 39), a window frame takes 9 ms and a draw 16 ms, and the worst gap, 115 ms, is the
/// opened note's highlighting (NOTEPAD, PERF). Grown (926, 3457) it shows in 72 ms. The whole
/// vault (7711, 48889) shows in 283 ms, its answer landing on the main loop in 1.3 ms with a worst
/// gap of 29 ms, and its tags (8017, 81193) shown or hidden again land in 4 ms. With animations
/// off round the note it shows in 45 ms, and the whole vault in 226 ms. The pointer over a node
/// of the settled graph has its picture on screen in 36 ms round the note and in 133 ms for the
/// whole vault. At 2× (`GDK_SCALE=2`, three runs) a draw takes three times as long (51 ms round
/// the note, 278 for the whole vault) and a window frame up to 75–98 ms. RSS stays at 270–310 MB.
/// Drawn with cairo under Xvfb, so a frame costs more than a GL renderer's.
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
    // What the vault's answers cost on their worker, which each first paint waits for, five times
    // each.
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    let roots = vec![best.clone()];
    let grown_roots: Vec<String> = roots.iter().cloned().chain(grow_by.clone()).collect();
    let shown = accent_core::graph::Shown::default();
    let timed = work::off_thread("graph", move || {
        let timed = |f: &dyn Fn() -> String| {
            let mut ms = Vec::new();
            let mut answer = String::new();
            for _ in 0..5 {
                let t = Instant::now();
                answer = f();
                ms.push(ms_since(t));
            }
            (spread(ms), answer)
        };
        let around = timed(&|| match vault.graph_around(&roots, shown) {
            Ok(part) => digest(&part),
            Err(e) => format!("error:{e}"),
        });
        let grown = timed(&|| match vault.graph_around(&grown_roots, shown) {
            Ok(part) => digest(&part),
            Err(e) => format!("error:{e}"),
        });
        let all = timed(&|| match vault.graph_all(shown) {
            Ok(part) => digest(&part),
            Err(e) => format!("error:{e}"),
        });
        // And with tags and images shown, once.
        let every = accent_core::graph::Shown {
            tags: true,
            images: true,
            ..shown
        };
        let tagged = match vault.graph_around(&grown_roots, every) {
            Ok(part) => digest(&part),
            Err(e) => format!("error:{e}"),
        };
        ([around, grown, all], tagged)
    })
    .await;
    if let Some(([around, grown, all], tagged)) = timed {
        println!(
            "bench graph vault scale={} around_ms={} grown_ms={} all_ms={} around={} grown={} \
             all={} grown_tagged={tagged}",
            app.window.scale_factor(),
            around.0,
            grown.0,
            all.0,
            around.1,
            grown.1,
            all.1
        );
    }
    concurrent(app, &best).await;
    app.open_path(&best);
    until("graph", "note", 30_000, || app.tab_for(&best).is_some()).await;
    // The note's text view lays out and highlights in idle callbacks at first, holding the loop
    // up to ~190 ms (NOTEPAD, PERF): the graph is timed once that is done.
    glib::timeout_future(Duration::from_secs(2)).await;
    let budget = |first_ms, frame_ms, worst_ms, frames| Budget {
        first_ms,
        frame_ms,
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
    let all = || budget(Some(500.0), Some(16.0), 50.0, Frames::Any);
    measure(app, "all", all(), || command(app, "win.graph-all")).await;
    input(app, "vault", || {
        command(app, "win.graph-reset");
        command(app, "win.graph-all");
    })
    .await;
    measure(app, "all_tags", all(), || command(app, "win.graph-tags")).await;
    measure(app, "all_untagged", all(), || {
        command(app, "win.graph-tags")
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
    hover(app, "local").await;
    let still = budget(Some(500.0), None, 50.0, Frames::AtMost(2));
    measure(app, "all_still", still, || command(app, "win.graph-all")).await;
    hover(app, "all").await;
}

/// `concurrent`: what the Search pane, the file tree and the backlinks of `best` wait while the
/// graph's question is on its way, each alone and then started 2 ms after the graph round `best`
/// or the whole vault's, five times each, as `median/p90/max` in ms.
async fn concurrent(app: &Rc<App>, best: &str) {
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    let best = best.to_string();
    let lines = work::off_thread("graph", move || {
        let word = accent_core::path::basename(&best);
        let word = word.split('.').next().unwrap_or(word).to_string();
        let shown = accent_core::graph::Shown::default();
        let roots = vec![best.clone()];
        let calls: [(&str, &(dyn Fn() + Sync)); 3] = [
            ("search", &|| {
                let _ = vault.search(&word, 50, false);
            }),
            ("list", &|| {
                let _ = vault.list_dir("");
            }),
            ("backlinks", &|| {
                let _ = vault.backlinks(&best);
            }),
        ];
        let mut lines = Vec::new();
        for during in ["alone", "around", "all"] {
            let mut line = format!("bench graph concurrent {during}");
            for (name, call) in calls {
                let mut ms = Vec::new();
                for _ in 0..5 {
                    std::thread::scope(|s| {
                        match during {
                            "around" => {
                                s.spawn(|| vault.graph_around(&roots, shown));
                            }
                            "all" => {
                                s.spawn(|| vault.graph_all(shown));
                            }
                            _ => {}
                        }
                        if during != "alone" {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        let t = Instant::now();
                        call();
                        ms.push(ms_since(t));
                    });
                }
                line += &format!(" {name}_ms={}", spread(ms));
            }
            lines.push(line);
        }
        lines
    })
    .await;
    for line in lines.unwrap_or_default() {
        println!("{line}");
    }
}

/// `input`: `act`, a cold layout round the file in front or the whole vault, then once its answer
/// landed a pan, a zoom or the pointer over a node every 50 ms while it settles and draws, 30 in
/// all, so the inputs meet the same layout however fast the vault answered. Prints the main
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
    until("graph", "input landed", 30_000, || graph.landed()).await;
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
    println!(
        "bench graph input {phase} scale={} inputs={} worst_ms={worst:.0} shown={} shown_ms={} \
         drawn={} drawn_ms={} nodes={}",
        app.window.scale_factor(),
        inputs.len(),
        shown.len(),
        spread(shown.clone()),
        drawn.len(),
        spread(drawn.clone()),
        graph.node_count()
    );
}

/// `hover`: the pointer over one node of the settled graph after another, 20 in all, each once
/// the last one's picture was on screen. Prints how long until the worker's picture of each was
/// (`drawn_ms`), and the main loop's worst gap meanwhile; a node the pointer finds lit already
/// asks for no picture and is left out.
async fn hover(app: &Rc<App>, case: &str) {
    let Some(graph) = app.graphs().into_iter().next() else {
        return;
    };
    let Some(clock) = graph.frame_clock() else {
        return;
    };
    let frames = Rc::new(RefCell::new(Vec::<(Instant, u64)>::new()));
    let hook = clock.connect_after_paint({
        let (frames, graph) = (frames.clone(), graph.clone());
        move |_| {
            frames
                .borrow_mut()
                .push((Instant::now(), graph.serials().1))
        }
    });
    let beat = super::native::Beat::start();
    let mut drawn = Vec::new();
    for k in 0..20 {
        let before = graph.serials().0;
        let t = Instant::now();
        graph.bench_input(3 * k + 2);
        // The view is asked of the worker as the next frame is drawn.
        let mut asked = None;
        while asked.is_none() && t.elapsed() < Duration::from_millis(500) {
            glib::timeout_future(Duration::from_millis(1)).await;
            asked = Some(graph.serials().0).filter(|&s| s > before);
        }
        let Some(asked) = asked else {
            continue;
        };
        let shown = until("graph", "hover drawn", 5_000, || {
            (frames.borrow().iter())
                .find(|f| f.0 > t && f.1 >= asked)
                .map(|f| f.0)
        })
        .await;
        if let Some(at) = shown {
            drawn.push((at - t).as_secs_f64() * 1e3);
        }
    }
    let (worst, _) = beat.stop();
    clock.disconnect(hook);
    println!(
        "bench graph hover {case} scale={} hovers={} drawn_ms={} worst_ms={worst:.0} nodes={}",
        app.window.scale_factor(),
        drawn.len(),
        spread(drawn.clone()),
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
    let (was, asked) = match app.graphs().first() {
        Some(graph) => {
            graph.take_paints();
            graph.take_draws();
            graph.take_landing_ms();
            (graph.node_count(), graph.questions())
        }
        None => (0, 0),
    };
    println!("bench graph {case} start");
    let beat = super::native::Beat::start();
    let cpu = super::native::schedstat();
    let t = Instant::now();
    act();
    // The command itself, on the main loop: a new tab and the window's syncs for the first.
    let act_ms = ms_since(t);
    let frame_ms = Rc::new(RefCell::new(Vec::new()));
    // The first frame presented once a picture of what is shown went into a snapshot.
    let first = Rc::new(Cell::new(None::<f64>));
    let mut hooked = None;
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
                let (first, shown) = (first.clone(), Rc::downgrade(graph));
                let after = clock.connect_after_paint(move |_| {
                    let drawn = (shown.upgrade()).and_then(|g| g.shown_at());
                    if first.get().is_none() && drawn.is_some_and(|at| at > t) {
                        first.set(Some(ms_since(t)));
                    }
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
            if graph.laid_out() && first.get().is_some() {
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
    let snapshots = graph.take_paints();
    let frames_ms = frame_ms.take();
    let frame_median = median(frames_ms.clone());
    let since = |at: Option<Instant>| at.map_or(-1.0, |at| (at - t).as_secs_f64() * 1000.0);
    // The first paint split into the vault's answer landing and the rest, its first picture.
    let query_ms = since(graph.landed_at());
    let first_snapshot_ms = since(graph.shown_at().filter(|&at| at > t));
    let (landing_ms, diff_ms) = graph.take_landing_ms();
    let frames = graph.frames();
    let first = first.get();
    let ok = settled.is_some()
        && budget
            .first_ms
            .is_none_or(|ms| first.is_some_and(|f| f <= ms))
        && budget.frame_ms.is_none_or(|ms| frame_median <= ms)
        && worst <= budget.worst_ms
        && match budget.frames {
            Frames::AtLeast(n) => frames >= n,
            Frames::AtMost(n) => frames <= n,
            Frames::Any => true,
        };
    println!(
        "bench graph {case} scale={} was={was} nodes={} links={} first_paint_ms={:.0} \
         first_snapshot_ms={first_snapshot_ms:.0} query_ms={query_ms:.0} settle_ms={:.0} \
         frames={frames} snapshot_ms={} frame_ms={} draw_ms={} act_ms={act_ms:.0} \
         landing_ms={landing_ms:.1} diff_ms={diff_ms:.1} queries={} worst_ms={worst:.0} \
         busy_ms={busy:.0} cpu_ms={cpu:.0} wait_ms={wait:.0} rss_mb={} budget={}",
        app.window.scale_factor(),
        graph.node_count(),
        graph.links(),
        first.unwrap_or(-1.0),
        settled.unwrap_or(-1.0),
        spread(snapshots),
        spread(frames_ms),
        spread(graph.take_draws()),
        graph.questions() - asked,
        super::notebook::rss_mb(),
        if ok { "ok" } else { "over" }
    );
}
