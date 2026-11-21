//! The vault as a graph of files and what joins them, as the Graph tab draws it (DESIGN.md,
//! Graph). The index answers a [`Part`] of it (`Index::graph_around`, `Index::graph_all`) and
//! [`layout`] places its nodes; neither knows anything of how they are painted.

use crate::query::Doc;
use serde::{Deserialize, Serialize};

pub mod layout;

/// What a node stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    /// A file the index holds, with what `kind:` calls it (`None` for a name none of the kinds is).
    File(Option<Doc>),
    /// A note links name that is not there, keyed by the path New File would write
    /// ([`crate::path::linked_path`]).
    Missing,
    /// A tag, keyed by `#` and its name as written: `#Rust` and `#rust` are two nodes.
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    /// The file's vault path, a missing note's path, or `#tag`.
    pub key: String,
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    /// A wikilink or a markdown link.
    Link,
    /// `![[…]]`.
    Embed,
    /// A code file's import of another.
    Import,
    /// A file carrying a tag, from the file to the tag.
    Tag,
}

/// From the node that writes the link, import or tag to the one it names, as indices into
/// [`Part::nodes`], which keeps a remote answer a third the size of one naming both ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub from: u32,
    pub to: u32,
    pub kind: EdgeKind,
}

/// What the graph's menu shows besides the files and their links and imports. Read with the
/// fields a caller leaves out as the defaults, so an older session or rpc call still reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shown {
    /// Notes links name that are not there, as ghost nodes.
    pub unresolved: bool,
    /// A node per tag, joined to the files carrying it.
    pub tags: bool,
    pub images: bool,
    /// Files nothing joins to anything, in the whole vault's graph.
    pub orphans: bool,
}

/// The user's defaults (decisions 2026-11-21): ghosts shown, the rest a toggle away.
impl Default for Shown {
    fn default() -> Self {
        Shown {
            unresolved: true,
            tags: false,
            images: false,
            orphans: false,
        }
    }
}

/// Some nodes of the graph and every edge among them, each edge once per `(from, to, kind)` and
/// none from a node to itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Part {
    /// Keep the nodes `keep` takes, in their order, and the edges between two of them, renumbered.
    pub fn retain(&mut self, keep: impl Fn(usize, &Node) -> bool) {
        let mut new = vec![u32::MAX; self.nodes.len()];
        let mut at = 0;
        for (i, node) in self.nodes.iter().enumerate() {
            if keep(i, node) {
                new[i] = at;
                at += 1;
            }
        }
        let mut i = 0;
        self.nodes.retain(|_| {
            i += 1;
            new[i - 1] != u32::MAX
        });
        self.edges.retain_mut(|e| {
            (e.from, e.to) = (new[e.from as usize], new[e.to as usize]);
            e.from != u32::MAX && e.to != u32::MAX
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: &str) -> Node {
        Node {
            key: key.into(),
            kind: NodeKind::File(Some(Doc::Note)),
        }
    }

    fn edge(from: u32, to: u32) -> Edge {
        Edge {
            from,
            to,
            kind: EdgeKind::Link,
        }
    }

    #[test]
    fn retain_renumbers_the_edges_it_keeps() {
        let mut part = Part {
            nodes: vec![node("a"), node("b"), node("c")],
            edges: vec![edge(0, 1), edge(1, 2), edge(2, 0)],
        };
        part.retain(|_, n| n.key != "b");
        assert_eq!(part.nodes, [node("a"), node("c")]);
        assert_eq!(part.edges, [edge(1, 0)]);
    }

    /// The wire form a remote vault and the GTK side read, and a session's fields read as the
    /// defaults where it has none.
    #[test]
    fn kinds_and_shown_read_as_the_wire_writes_them() {
        let kinds = [NodeKind::File(Some(Doc::Note)), NodeKind::File(None)];
        let json = serde_json::to_string(&kinds).unwrap();
        assert_eq!(json, r#"[{"File":"Note"},{"File":null}]"#);
        assert_eq!(
            serde_json::to_string(&NodeKind::Missing).unwrap(),
            r#""Missing""#
        );
        let shown: Shown = serde_json::from_str(r#"{"tags":true}"#).unwrap();
        assert_eq!(
            shown,
            Shown {
                tags: true,
                ..Shown::default()
            }
        );
        assert!(shown.unresolved);
    }
}
