//! The graph the canvas shows: the vault's answer kept in the order the force layout holds its
//! nodes, and what each new answer changes in it. Pure, so it is tested without a window.
//!
//! An answer is a whole [`Part`]; [`Graph::changes`] diffs it by key against what is shown, so a
//! node that stays keeps its index, and with it its place in the layout.

use std::collections::{HashMap, HashSet};

use accent_core::graph::{Edge, EdgeKind, Node, NodeKind, Part};
use accent_core::query::Doc;

#[derive(Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Each node's neighbours, by links either way, each once.
    pub adjacency: Vec<Vec<usize>>,
    /// The keys the graph grew from: the file in front and every node clicked since.
    pub roots: Vec<String>,
    index: HashMap<String, usize>,
}

/// What a new answer changes, in the order the canvas and the layout apply it.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    /// The nodes no longer shown, highest index first: each taken out as `Vec::swap_remove`
    /// does, the last node moving into its place, so every index is still its node's when its
    /// turn comes.
    pub removed: Vec<usize>,
    /// The nodes new to the graph, appended after the removals, each with a node shown before
    /// that it links to, which it is placed beside.
    pub added: Vec<(Node, Option<usize>)>,
    /// A node whose kind changed in place: a ghost whose note was written.
    pub kinds: Vec<(usize, NodeKind)>,
    /// Every edge, by the indices after the change.
    pub edges: Vec<Edge>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty() && self.kinds.is_empty()
    }
}

/// How a node is painted (user decision 2026-11-21): the canvas turns each into the theme's
/// colours as it draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Paint {
    /// A note, in the foreground.
    Ink,
    /// Code, a PDF, a diagram, a notebook or an image, in one of the five hues after the accent
    /// (`highlight::lane_colour`).
    Lane(usize),
    /// Text, or a file of no kind, in the foreground at 60 %.
    Faint,
    /// A tag, a square in the foreground at 60 %.
    Tag,
    /// A note not written yet, a ring in the foreground at 40 %.
    Ghost,
}

pub(super) fn paint_of(kind: NodeKind) -> Paint {
    match kind {
        NodeKind::File(Some(Doc::Note)) => Paint::Ink,
        NodeKind::File(Some(Doc::Code)) => Paint::Lane(1),
        NodeKind::File(Some(Doc::Pdf)) => Paint::Lane(2),
        NodeKind::File(Some(Doc::Diagram)) => Paint::Lane(3),
        NodeKind::File(Some(Doc::Notebook)) => Paint::Lane(4),
        NodeKind::File(Some(Doc::Image)) => Paint::Lane(5),
        NodeKind::File(Some(Doc::Text) | None) => Paint::Faint,
        NodeKind::Tag => Paint::Tag,
        NodeKind::Missing => Paint::Ghost,
    }
}

/// A node's radius in the layout's units, growing with its links: 4 alone, 12 at most.
pub(super) fn radius(degree: usize) -> f32 {
    (4.0 + 1.5 * (degree as f32).sqrt()).min(12.0)
}

/// What a node is called on the canvas and in find: a file's whole name, `.md` and all, and a
/// tag as written.
pub(super) fn label(node: &Node) -> &str {
    match node.kind {
        NodeKind::Tag => &node.key,
        _ => accent_core::path::basename(&node.key),
    }
}

impl Graph {
    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.index.get(key).copied()
    }

    /// What it takes to show `part`: every node of it, a node shown already keeping its index.
    /// A new node is placed beside the one keyed `near` where it links to that, as what a click
    /// adds slides out of the node clicked, else beside any node it links to.
    pub fn changes(&self, part: &Part, near: Option<&str>) -> Changes {
        let wanted: HashSet<&str> = part.nodes.iter().map(|n| n.key.as_str()).collect();
        let mut removed: Vec<usize> = (self.nodes.iter().enumerate())
            .filter(|(_, n)| !wanted.contains(n.key.as_str()))
            .map(|(i, _)| i)
            .collect();
        removed.reverse();
        // Where each node left ends up once the removals have moved the last ones down.
        let mut order: Vec<usize> = (0..self.nodes.len()).collect();
        for &i in &removed {
            order.swap_remove(i);
        }
        let mut moved = vec![usize::MAX; self.nodes.len()];
        for (at, &was) in order.iter().enumerate() {
            moved[was] = at;
        }
        let mut changes = Changes {
            removed,
            ..Changes::default()
        };
        // Each of the part's nodes' index after the change, and whether it was shown before.
        let mut new = Vec::with_capacity(part.nodes.len());
        for node in &part.nodes {
            match self.index_of(&node.key) {
                Some(was) => {
                    if self.nodes[was].kind != node.kind {
                        changes.kinds.push((moved[was], node.kind));
                    }
                    new.push((moved[was], true));
                }
                None => new.push((
                    order.len() + new.iter().filter(|(_, old)| !old).count(),
                    false,
                )),
            }
        }
        let mut neighbours = vec![Vec::new(); part.nodes.len()];
        for e in &part.edges {
            neighbours[e.from as usize].push(e.to as usize);
            neighbours[e.to as usize].push(e.from as usize);
        }
        let near = near.and_then(|key| part.nodes.iter().position(|n| n.key == key));
        for (i, node) in part.nodes.iter().enumerate() {
            if new[i].1 {
                continue;
            }
            // A node the layout holds by the time this one is added: one shown before, or one
            // added ahead of it.
            let placed = |j: usize| new[j].1 || new[j].0 < new[i].0;
            let beside = (near.filter(|&n| placed(n) && neighbours[i].contains(&n)))
                .or_else(|| neighbours[i].iter().copied().find(|&j| new[j].1))
                .or_else(|| neighbours[i].iter().copied().find(|&j| placed(j)));
            changes.added.push((node.clone(), beside.map(|j| new[j].0)));
        }
        changes.edges = (part.edges.iter())
            .map(|e| Edge {
                from: new[e.from as usize].0 as u32,
                to: new[e.to as usize].0 as u32,
                kind: e.kind,
            })
            .collect();
        changes
    }

    /// Take `changes` in: the removals in their order, the kinds, then the new nodes.
    pub fn apply(&mut self, changes: &Changes) {
        for &i in &changes.removed {
            self.nodes.swap_remove(i);
        }
        for &(i, kind) in &changes.kinds {
            self.nodes[i].kind = kind;
        }
        self.nodes
            .extend(changes.added.iter().map(|(node, _)| node.clone()));
        self.edges = changes.edges.clone();
        self.reindex();
    }

    /// A file or a folder moved from `from` to `to`: its nodes, and the roots, keep their place
    /// under their new keys.
    pub fn rename(&mut self, from: &str, to: &str) {
        let moved = |key: &str| match key.strip_prefix(from) {
            Some("") => Some(to.to_string()),
            Some(rest) if rest.starts_with('/') => Some(format!("{to}{rest}")),
            _ => None,
        };
        for root in &mut self.roots {
            if let Some(new) = moved(root) {
                *root = new;
            }
        }
        let mut any = false;
        for node in &mut self.nodes {
            if let Some(new) = (node.kind != NodeKind::Tag)
                .then(|| moved(&node.key))
                .flatten()
            {
                node.key = new;
                any = true;
            }
        }
        if any {
            self.reindex();
        }
    }

    /// A file or a folder deleted: no root is in it any more.
    pub fn forget(&mut self, gone: &str) {
        self.roots.retain(|r| {
            r != gone
                && !r
                    .strip_prefix(gone)
                    .is_some_and(|rest| rest.starts_with('/'))
        });
    }

    /// The pairs the layout pulls together, each once whichever way and however many kinds of
    /// edge join them.
    pub fn links(&self) -> Vec<(usize, usize)> {
        let mut seen = HashSet::new();
        (self.edges.iter())
            .map(|e| (e.from.min(e.to) as usize, e.from.max(e.to) as usize))
            .filter(|pair| seen.insert(*pair))
            .collect()
    }

    /// "42 files · 57 links": the files among the nodes and the links and imports among them, a
    /// tag's edges being no link.
    pub fn facts(&self) -> String {
        let files = (self.nodes.iter())
            .filter(|n| matches!(n.kind, NodeKind::File(_)))
            .count();
        let links = (self.edges.iter())
            .filter(|e| e.kind != EdgeKind::Tag)
            .count();
        let plural = |n: usize, one: &str| match n {
            1 => format!("1 {one}"),
            n => format!("{n} {one}s"),
        };
        format!("{} · {}", plural(files, "file"), plural(links, "link"))
    }

    fn reindex(&mut self) {
        self.index = (self.nodes.iter().enumerate())
            .map(|(i, n)| (n.key.clone(), i))
            .collect();
        self.adjacency = vec![Vec::new(); self.nodes.len()];
        for (a, b) in self.links() {
            self.adjacency[a].push(b);
            self.adjacency[b].push(a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(key: &str) -> Node {
        Node {
            key: key.into(),
            kind: NodeKind::File(Some(Doc::Note)),
        }
    }

    fn link(from: u32, to: u32) -> Edge {
        Edge {
            from,
            to,
            kind: EdgeKind::Link,
        }
    }

    fn part(keys: &[&str], edges: &[(u32, u32)]) -> Part {
        Part {
            nodes: keys.iter().map(|k| note(k)).collect(),
            edges: edges.iter().map(|&(a, b)| link(a, b)).collect(),
        }
    }

    fn keys(graph: &Graph) -> Vec<&str> {
        graph.nodes.iter().map(|n| n.key.as_str()).collect()
    }

    fn shown(first: &Part) -> Graph {
        let mut graph = Graph::default();
        graph.apply(&graph.changes(first, None));
        graph
    }

    /// The first answer is all new, in its own order, and its edges as they came.
    #[test]
    fn a_first_answer_is_added_whole() {
        let graph = shown(&part(&["a.md", "b.md", "c.md"], &[(0, 1), (1, 2)]));
        assert_eq!(keys(&graph), ["a.md", "b.md", "c.md"]);
        assert_eq!(graph.adjacency[1], [0, 2]);
        assert_eq!(graph.index_of("c.md"), Some(2));
        assert_eq!(graph.facts(), "3 files · 2 links");
    }

    /// Survivors keep their index, a removal moves the last node into its place, and what is
    /// new is appended beside the node clicked.
    #[test]
    fn an_answer_keeps_the_survivors_where_they_were() {
        let mut graph = shown(&part(&["a.md", "b.md", "c.md", "d.md"], &[(0, 1)]));
        let next = part(&["d.md", "a.md", "e.md", "c.md"], &[(1, 2), (2, 3)]);
        let changes = graph.changes(&next, Some("c.md"));
        // b.md goes, and d.md, last, takes its index.
        assert_eq!(changes.removed, [1]);
        assert_eq!(changes.added, [(note("e.md"), Some(2))]);
        graph.apply(&changes);
        assert_eq!(keys(&graph), ["a.md", "d.md", "c.md", "e.md"]);
        assert_eq!(changes.edges, [link(0, 3), link(3, 2)]);
        assert_eq!(
            graph.changes(&next, None),
            Changes {
                edges: changes.edges.clone(),
                ..Changes::default()
            }
        );
    }

    /// Several removals, each index its node's when its turn comes.
    #[test]
    fn removals_go_highest_first() {
        let mut graph = shown(&part(&["a", "b", "c", "d", "e"], &[]));
        let changes = graph.changes(&part(&["e", "c"], &[]), None);
        assert_eq!(changes.removed, [3, 1, 0]);
        graph.apply(&changes);
        assert_eq!(keys(&graph), ["c", "e"]);
    }

    /// A ghost whose note was written is the same node, a file now.
    #[test]
    fn a_ghost_turns_into_a_file_in_place() {
        let mut first = part(&["a.md", "gone.md"], &[(0, 1)]);
        first.nodes[1].kind = NodeKind::Missing;
        let mut graph = shown(&first);
        let changes = graph.changes(&part(&["a.md", "gone.md"], &[(0, 1)]), None);
        assert_eq!(changes.kinds, [(1, NodeKind::File(Some(Doc::Note)))]);
        assert!(changes.removed.is_empty() && changes.added.is_empty());
        graph.apply(&changes);
        assert_eq!(paint_of(graph.nodes[1].kind), Paint::Ink);
    }

    #[test]
    fn a_rename_keeps_the_index_and_the_root() {
        let mut graph = shown(&part(&["a.md", "b.md", "dir/c.md", "dirt.md"], &[(0, 1)]));
        graph.roots = vec!["b.md".into(), "dir/c.md".into()];
        graph.rename("b.md", "bee.md");
        assert_eq!(graph.index_of("bee.md"), Some(1));
        assert_eq!(graph.index_of("b.md"), None);
        // A folder takes what is in it along, and nothing that only starts like it.
        graph.rename("dir", "box");
        assert_eq!(keys(&graph), ["a.md", "bee.md", "box/c.md", "dirt.md"]);
        assert_eq!(graph.roots, ["bee.md", "box/c.md"]);
        // The next answer names them under their new keys, and nothing moves.
        let next = part(&["a.md", "bee.md", "box/c.md", "dirt.md"], &[(0, 1)]);
        assert!(graph.changes(&next, None).is_empty());
        graph.forget("box");
        assert_eq!(graph.roots, ["bee.md"]);
    }

    /// A link and an embed between the same two files pull them together once.
    #[test]
    fn the_layout_gets_each_pair_once() {
        let mut both = part(&["a.md", "b.md"], &[(0, 1), (1, 0)]);
        both.edges[1].kind = EdgeKind::Embed;
        assert_eq!(shown(&both).links(), [(0, 1)]);
    }

    #[test]
    fn a_tag_is_named_as_written_and_a_file_by_its_name() {
        let tag = Node {
            key: "#Rust".into(),
            kind: NodeKind::Tag,
        };
        assert_eq!(label(&tag), "#Rust");
        assert_eq!(label(&note("Inbox/today.md")), "today.md");
        assert_eq!(radius(0), 4.0);
        assert_eq!(radius(1000), 12.0);
    }
}
