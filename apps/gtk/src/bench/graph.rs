//! The Graph tab: what it shows around the file in front, where a press on a node opens that
//! file and where the keyboard is left, and the tab coming back with the session.

use super::*;
use crate::graph::GraphTab;

/// `ACCENT_BENCH_GRAPH=check:<rel>` opens `rel`, then Show Graph, and prints the nodes, a ghost's
/// key ending in `?`, and the links once laid out (`open`). It presses two of them in turn, each
/// opening beside the graph as the other pane's preview, and prints where each opened and where the
/// keyboard is (`click`), and the roots and nodes the graph grew to (`grow`). Then it saves the
/// session and quits. `restored` prints whether the graph came back with the session and the file
/// it is around (`session`); opens `lone.md`, which it does not hold, and with animations off shows
/// the graph again, printing what it started again around and whether it showed no places between
/// where the nodes were placed and the settled layout (`still`); then closes it and prints whether
/// the layout's threads ended with it (`closed`). `drill_graph` writes the notes into a scratch
/// vault.
pub(super) fn bench_graph(app: &Rc<App>, arg: &str) {
    let app = app.clone();
    let arg = arg.to_string();
    glib::spawn_future_local(async move {
        until("graph", "reconciled", 60_000, || app.reconciled.get()).await;
        match arg.split_once(':') {
            Some(("check", rel)) => check(&app, rel).await,
            None if arg == "restored" => restored(&app).await,
            _ => eprintln!("ACCENT_BENCH_GRAPH=check:<rel> or restored"),
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
        let keyboard = match app.focused() {
            Some(focus) if focus.is_ancestor(&graph.page.child()) => "graph",
            Some(_) => "elsewhere",
            None => "none",
        };
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
    app.save_session();
}

async fn restored(app: &Rc<App>) {
    let Some(graph) = laid_out(app).await else {
        return;
    };
    println!(
        "bench graph session restored=true front={}",
        graph.front().unwrap_or_default()
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
