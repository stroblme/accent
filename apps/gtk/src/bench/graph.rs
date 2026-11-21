//! The Graph tab: what it shows around the file in front, where a press on a node opens that
//! file and where the keyboard is left, and the tab coming back with the session.

use super::*;
use crate::graph::GraphTab;

/// `ACCENT_BENCH_GRAPH=check:<rel>` opens `rel`, then Show Graph, and prints the nodes, a ghost's
/// key ending in `?`, and the links once laid out (`open`). It presses two of them in turn, each
/// opening beside the graph as the other pane's preview, and prints where each opened and where
/// the keyboard is (`click`); then saves the session and quits. `restored` prints whether the
/// graph came back with the session and the file it is around (`session`), closes it and prints
/// that it went (`closed`). `drill_graph` writes the notes into a scratch vault.
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
    app.close_page(&graph.page);
    until("graph", "closed", 10_000, || app.graphs().is_empty()).await;
    println!("bench graph closed");
}

/// The window's graph once it shows a laid-out answer.
async fn laid_out(app: &Rc<App>) -> Option<Rc<GraphTab>> {
    let graph = until("graph", "graph", 10_000, || app.graphs().into_iter().next()).await?;
    until("graph", "laid out", 30_000, || graph.laid_out()).await?;
    Some(graph)
}
