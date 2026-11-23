//! The Graph tab's find bar (DESIGN.md, Graph; user decisions 2026-11-23): the Search pane's
//! query, asked of the vault once a query (`Vault::search_files`) off the main loop, the local
//! graph's files kept to the graph's own and the whole vault's not. Every node it does not answer
//! is hidden ([`GraphView::set_filter`]) and the view fits what is left; a query of plain words
//! also finds a node by its name, a tag and a ghost among them, after the files the vault ranks.
//! Enter steps through what it found best first, each the node the keyboard is on, kept in the
//! middle while the layout settles, and the first Enter takes the best.

use std::collections::HashSet;

use super::*;
use accent_core::query;
use model::Graph;

/// The bar's query over the graph and what it found.
#[derive(Default)]
pub(super) struct Find {
    /// The query as typed, and why it cannot be read where it cannot.
    query: String,
    why: Option<String>,
    /// The last query that could be read and said anything, whose answer is shown or on its way:
    /// one that cannot be read leaves it as it is.
    asked: Option<String>,
    /// The words a node's name holds to be found by it, for a query of plain words.
    words: Vec<String>,
    /// The nodes found, best first, and the one the last step went to.
    hits: Vec<String>,
    current: Option<usize>,
    /// Whether the answer to `asked` has landed, and the step pressed before it did, forward or
    /// back, which it takes once it lands.
    landed: bool,
    pending: Option<bool>,
}

impl GraphTab {
    /// Forget the query, the bar being put away: every node is shown again where it was and the
    /// view from before the query put back, the keyboard staying on the node found.
    pub fn find_clear(&self) {
        *self.finding.borrow_mut() = Find::default();
        // An answer on its way lands no more.
        self.found.set(self.found.get() + 1);
        self.view.set_filter(None, false);
    }

    /// Whether a query hides nodes, or is about to, so the graph asks it again as it changes.
    fn querying(&self) -> bool {
        self.finding.borrow().asked.is_some()
    }

    /// The next hit or the one before, round past either end, the first step taking the best or,
    /// backwards, the last: the node the keyboard is on from then, brought to the middle.
    fn step_match(&self, forward: bool) -> Option<String> {
        let key = {
            let mut find = self.finding.borrow_mut();
            let total = find.hits.len();
            if total == 0 {
                return None;
            }
            let at = match (find.current, forward) {
                (None, true) => 0,
                (None, false) => total - 1,
                (Some(at), true) => (at + 1) % total,
                (Some(at), false) => (at + total - 1) % total,
            };
            find.current = Some(at);
            find.hits[at].clone()
        };
        self.view.set_current(&key, true);
        Some(key)
    }

    /// "3 of 12", what to say when there is nothing to show, or nothing while the answer to a
    /// new query is on its way, which leaves the readout as it was.
    fn matches_label(&self) -> Option<String> {
        let find = self.finding.borrow();
        if find.why.is_some() {
            return Some("Invalid query".to_string());
        }
        if find.asked.is_none() {
            return Some(String::new());
        }
        find.landed.then(|| match (find.hits.len(), find.current) {
            (0, _) => "No results".to_string(),
            (total, None) => format!("{total} matches"),
            (total, Some(at)) => format!("{} of {total}", at + 1),
        })
    }
}

impl App {
    /// The bar's query typed over `graph`, read here first: one that cannot be read says why
    /// and leaves the graph as the last one left it, one that says nothing shows every node
    /// again, and any other is asked of the vault.
    pub(crate) fn graph_find(self: &Rc<Self>, graph: &Rc<GraphTab>, text: &str) {
        if graph.finding.borrow().query == text {
            return;
        }
        let read = query::parse(text, &chrono::Local::now());
        graph.finding.borrow_mut().query = text.to_string();
        let q = match read {
            Ok(q) => q,
            Err(why) => {
                graph.finding.borrow_mut().why = Some(why.to_string());
                return;
            }
        };
        if q.text.is_empty() && q.filters.is_empty() {
            graph.find_clear();
            graph.finding.borrow_mut().query = text.to_string();
            return;
        }
        // Plain words find a node by its name too.
        let words = match q.filters.is_empty() {
            true => q.text.iter().map(|t| t.text.to_lowercase()).collect(),
            false => Vec::new(),
        };
        {
            let mut find = graph.finding.borrow_mut();
            find.why = None;
            find.asked = Some(text.to_string());
            find.words = words;
            find.current = None;
            find.landed = false;
            find.pending = None;
        }
        self.ask_graph_find(graph, true);
    }

    /// Ask the vault for the files the query answers, off the main loop, the local graph only
    /// its own; once the newest answer lands, show what it found, the view fitted to it with
    /// `fit`. A graph grown or changed asks again without moving the view.
    fn ask_graph_find(self: &Rc<Self>, graph: &Rc<GraphTab>, fit: bool) {
        let query = graph.finding.borrow().asked.clone();
        let (Some(vault), Some(query)) = (self.vault().cloned(), query) else {
            return;
        };
        let asked = graph.found.get() + 1;
        graph.found.set(asked);
        let within = (!graph.all.get()).then(|| {
            let shown = graph.view.graph();
            let files = shown.nodes.iter();
            let files = files.filter(|n| matches!(n.kind, NodeKind::File(_)));
            files.map(|n| n.key.clone()).collect()
        });
        let (app, weak) = (Rc::downgrade(self), Rc::downgrade(graph));
        glib::spawn_future_local(async move {
            let answer =
                work::off_thread("graph find", move || vault.search_files(&query, within)).await;
            let (Some(app), Some(graph)) = (app.upgrade(), weak.upgrade()) else {
                return;
            };
            if graph.found.get() != asked {
                return;
            }
            #[cfg(feature = "bench")]
            graph.find_answered.set(asked);
            match answer {
                Some(Ok(files)) => app.land_graph_find(&graph, &files, fit),
                // The graph stays as the last answer left it.
                Some(Err(why)) => tracing::warn!("graph find: {why}"),
                None => {}
            }
        });
    }

    /// The vault's `files` for the query: the nodes it finds shown and the rest hidden, the step
    /// on the node found before kept there, and a step pressed meanwhile taken.
    fn land_graph_find(self: &Rc<Self>, graph: &Rc<GraphTab>, files: &[String], fit: bool) {
        let (keys, pending) = {
            let mut find = graph.finding.borrow_mut();
            let found = hits(files, &graph.view.graph(), &find.words);
            let was = find.current.and_then(|at| find.hits.get(at).cloned());
            find.current = was.and_then(|key| found.iter().position(|hit| *hit == key));
            find.hits = found;
            find.landed = true;
            let keys: HashSet<String> = find.hits.iter().cloned().collect();
            (keys, find.pending.take())
        };
        graph.view.set_filter(Some(keys), fit);
        if let Some(forward) = pending {
            self.graph_step(graph, forward);
        }
        self.graph_find_readout(graph);
    }

    /// Enter or Shift+Enter over `graph`: the next node found or the one before, held until the
    /// answer to a new query lands.
    pub(crate) fn graph_step(self: &Rc<Self>, graph: &Rc<GraphTab>, forward: bool) {
        {
            let mut find = graph.finding.borrow_mut();
            if find.asked.is_some() && !find.landed {
                find.pending = Some(forward);
                return;
            }
        }
        graph.step_match(forward);
    }

    /// The bar over `graph` says what it found and why a query cannot be read, while the graph
    /// is in front of its pane.
    pub(crate) fn graph_find_readout(&self, graph: &Rc<GraphTab>) {
        let Some(pane) = self.pane_of(&graph.page) else {
            return;
        };
        if !self.graph_of(&pane).is_some_and(|g| Rc::ptr_eq(&g, graph)) {
            return;
        }
        if let Some(text) = graph.matches_label() {
            pane.find.set_matches_text(&text);
        }
        pane.find
            .set_matches_tooltip(graph.finding.borrow().why.as_deref());
    }

    /// A new answer of the vault's landed on `graph` under a query: it is asked again for the
    /// nodes now shown, each new one hidden until then, the view left where it is unless the
    /// query's first answer, which fits it, was still on its way.
    pub(super) fn refind_graph(self: &Rc<Self>, graph: &Rc<GraphTab>) {
        if graph.querying() {
            let first = !graph.finding.borrow().landed;
            self.ask_graph_find(graph, first);
        }
    }
}

/// The nodes of `graph` a query finds: the files of the vault's `answer` it holds, best first,
/// then, for a query of plain `words`, every other node whose name holds each of them, by name.
fn hits(answer: &[String], graph: &Graph, words: &[String]) -> Vec<String> {
    let mut hits: Vec<String> = (answer.iter())
        .filter(|key| graph.index_of(key).is_some())
        .cloned()
        .collect();
    if words.is_empty() {
        return hits;
    }
    let found: HashSet<&str> = hits.iter().map(String::as_str).collect();
    let mut named: Vec<(String, &str)> = (graph.nodes.iter())
        .filter(|n| !found.contains(n.key.as_str()))
        .map(|n| (model::label(n).to_lowercase(), n.key.as_str()))
        .filter(|(name, _)| words.iter().all(|w| name.contains(w.as_str())))
        .collect();
    named.sort();
    let named: Vec<String> = named.into_iter().map(|(_, key)| key.to_string()).collect();
    hits.extend(named);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_core::graph::{Node, Part};
    use accent_core::query::Doc;

    /// The vault's files the graph holds come first in its order, then for plain words every
    /// other node whose name holds each word, a tag and a ghost too, by name.
    #[test]
    fn plain_words_find_a_node_by_its_name_after_the_files_ranked() {
        let node = |key: &str, kind| Node {
            key: key.into(),
            kind,
        };
        let file = NodeKind::File(Some(Doc::Note));
        let part = Part {
            nodes: vec![
                node("a.md", file),
                node("Bee.md", file),
                node("e.md", file),
                node("#bees", NodeKind::Tag),
                node("notes/beetle.md", NodeKind::Missing),
            ],
            edges: Vec::new(),
        };
        let mut graph = Graph::default();
        graph.apply(&graph.changes(&part, None));
        let answer = ["e.md".to_string(), "gone.md".into(), "a.md".into()];
        let words = |w: &[&str]| w.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(
            hits(&answer, &graph, &words(&["bee"])),
            ["e.md", "a.md", "#bees", "Bee.md", "notes/beetle.md"]
        );
        assert_eq!(
            hits(&answer, &graph, &words(&["bee", "tle"])),
            ["e.md", "a.md", "notes/beetle.md"]
        );
        assert_eq!(hits(&answer, &graph, &[]), ["e.md", "a.md"]);
    }
}
