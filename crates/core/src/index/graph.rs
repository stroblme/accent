//! The graph the Graph tab draws ([`crate::graph`]), read out of the links, imports and tags the
//! index holds: the files around some and every edge among them, or the whole vault's.

use super::Index;
use super::links::{DANGLING, ROW, missing_path};
use super::reconcile::link_kind_of;
use crate::Result;
use crate::code::is_test_path;
use crate::graph::{Edge, EdgeKind, Node, NodeKind, Part, Shown};
use crate::markdown::{LinkKind, link_key, strip_ext};
use crate::path::parent_dir;
use crate::query::Doc;
use crate::walk::FileKind;
use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

/// Whether the file `f` is one the graph shows at an edge's end, by search's rule: no folder or
/// conflict copy, nothing git ignores but a note, and nothing the index holds only because a tab
/// opened it (`keep_opened` clears its `git_ignored`); an image only with `images`.
fn shown_file(f: &str, images: bool) -> String {
    let [note, pdf, other] =
        [FileKind::Markdown, FileKind::Pdf, FileKind::Other].map(|k| k.as_i64());
    // No file's `doc` is -1.
    let image = if images { -1 } else { Doc::Image.as_i64() };
    format!(
        "{f}.kind IN ({note}, {pdf}, {other}) AND ({f}.git_ignored = 0 OR {f}.kind = {note})
         AND {f}.rel_path NOT IN (SELECT rel_path FROM opened) AND {f}.doc <> {image}"
    )
}

fn edge_kind(link_kind: i64) -> EdgeKind {
    match link_kind_of(link_kind) {
        LinkKind::Embed => EdgeKind::Embed,
        _ => EdgeKind::Link,
    }
}

/// Whether `rel` is a file of the Go package its folder is, which an import of the package names.
fn in_package(rel: &str) -> bool {
    rel.ends_with(".go") && !is_test_path(rel)
}

/// A key `graph_around` was handed, as the index reads it.
enum Seed {
    File(i64, String),
    Tag(String),
    Missing(String),
}

/// One link a file writes, as [`Index::links_out`] reads it.
struct Out {
    target: String,
    /// `(id, rel_path, doc)` of the file it resolves to.
    file: Option<(i64, String, i64)>,
    /// Whether that file is one [`shown_file`] takes.
    shown: bool,
    /// Whether it is [`DANGLING`], a missing note's ghost where [`missing_path`] names one.
    dangling: bool,
}

/// A [`Part`] as it is put together: each node once, a file by its row, a ghost by its key and a
/// tag by its name, and each edge once.
#[derive(Default)]
struct Builder {
    part: Part,
    files: HashMap<i64, u32>,
    missing: HashMap<String, u32>,
    tags: HashMap<String, u32>,
    edges: HashSet<Edge>,
    /// The ghosts keyed as they were handed in, which keep that spelling.
    pinned: HashSet<u32>,
}

/// The node `map` holds under `k`, pushed onto `nodes` as `node()` the first time.
fn once<K: Hash + Eq>(
    map: &mut HashMap<K, u32>,
    nodes: &mut Vec<Node>,
    k: K,
    node: impl FnOnce() -> Node,
) -> u32 {
    *map.entry(k).or_insert_with(|| {
        nodes.push(node());
        (nodes.len() - 1) as u32
    })
}

impl Builder {
    fn file(&mut self, (id, rel, doc): (i64, String, i64)) -> u32 {
        once(&mut self.files, &mut self.part.nodes, id, || Node {
            key: rel,
            kind: NodeKind::File(Doc::from_i64(doc)),
        })
    }

    /// The ghost for the missing note `rel`, one however its links spell it.
    fn missing(&mut self, rel: String) -> u32 {
        let key = link_key(&rel);
        if let Some(&i) = self.missing.get(&key) {
            self.respell(i, rel);
            return i;
        }
        once(&mut self.missing, &mut self.part.nodes, key, || Node {
            key: rel,
            kind: NodeKind::Missing,
        })
    }

    /// Ghost `i` keyed by `rel` where that spells it less: a ghost is keyed by the least spelling
    /// of its links whichever the answer meets first, so it keeps its key from answer to answer.
    fn respell(&mut self, i: u32, rel: String) {
        let node = &mut self.part.nodes[i as usize];
        if rel < node.key && !self.pinned.contains(&i) {
            node.key = rel;
        }
    }

    fn tag(&mut self, name: String) -> u32 {
        let key = format!("#{name}");
        once(&mut self.tags, &mut self.part.nodes, name, || Node {
            key,
            kind: NodeKind::Tag,
        })
    }

    fn edge(&mut self, from: u32, to: u32, kind: EdgeKind) {
        let edge = Edge { from, to, kind };
        if from != to && self.edges.insert(edge) {
            self.part.edges.push(edge);
        }
    }
}

type Row = (i64, String, i64);

/// A link as an edge needs it: its kind, the file it resolved to, or its target where it is
/// [`DANGLING`].
type Link = (i64, Option<i64>, Option<String>);

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
}

impl Index {
    /// The nodes around `keys`, one link, import or tag away either way, and every edge among
    /// them and the keys: what a Graph tab shows of the file in front and the nodes it has opened,
    /// the same for the same keys and index. A key is a file's path (a second path's answering as
    /// its row's), `#` and a tag's name, or a missing note's path, and is in the answer whatever
    /// `shown` leaves out; a neighbour is only where `shown` and [`shown_file`] take it. A ghost
    /// is keyed as in the whole vault's graph, by the least spelling of the links naming it.
    pub fn graph_around(&self, keys: &[String], shown: Shown) -> Result<Part> {
        let mut b = Builder::default();
        let mut seeds = Vec::new();
        for key in keys {
            let found = self
                .conn
                .prepare_cached(&format!(
                    "SELECT id, rel_path, doc FROM files WHERE id = {ROW}"
                ))?
                .query_row([key], row)
                .optional()?;
            seeds.push(match (found, key.strip_prefix('#')) {
                (Some(file), _) => {
                    let seed = Seed::File(file.0, file.1.clone());
                    b.file(file);
                    seed
                }
                (None, Some(name)) => {
                    b.tag(name.to_string());
                    Seed::Tag(name.to_string())
                }
                (None, None) => {
                    let ghost = b.missing(key.clone());
                    b.pinned.insert(ghost);
                    Seed::Missing(key.clone())
                }
            });
        }
        let filter = shown_file("f", shown.images);
        for seed in &seeds {
            match seed {
                Seed::File(id, rel) => {
                    for out in self.links_out(*id, shown.images)? {
                        match (out.file, out.dangling && shown.unresolved) {
                            (Some(file), _) if out.shown => {
                                b.file(file);
                            }
                            (None, true) => {
                                if let Some(rel) = missing_path(&out.target) {
                                    b.missing(rel);
                                }
                            }
                            _ => {}
                        }
                    }
                    let mut st = self.conn.prepare_cached(&format!(
                        "SELECT f.id, f.rel_path, f.doc FROM links l JOIN files f ON f.id = l.src_file
                         WHERE l.resolved_file = ?1 AND l.kind <> 3 AND {filter}"
                    ))?;
                    for file in st.query_map([id], row)? {
                        b.file(file?);
                    }
                    for (file, taken) in
                        (self.imports_out(&[*id], shown.images)?.remove(id)).unwrap_or_default()
                    {
                        if taken {
                            b.file(file);
                        }
                    }
                    for file in self.imports_in(*id, rel, shown.images)? {
                        b.file(file);
                    }
                    if shown.tags {
                        for name in self.tags_of_files(&[*id])?.remove(id).unwrap_or_default() {
                            b.tag(name);
                        }
                    }
                }
                Seed::Tag(name) => {
                    let mut st = self.conn.prepare_cached(&format!(
                        "SELECT DISTINCT f.id, f.rel_path, f.doc FROM tags t JOIN files f ON f.id = t.file_id
                         WHERE t.name = ?1 AND {filter}"
                    ))?;
                    for file in st.query_map([name], row)? {
                        b.file(file?);
                    }
                }
                // The notes whose dangling links name it, by either key a link to it can have:
                // looked up by key, where the planner would walk every dangling link instead.
                Seed::Missing(rel) => {
                    let mut st = self.conn.prepare_cached(&format!(
                        "SELECT s.id, s.rel_path, s.doc, l.target
                         FROM links l INDEXED BY idx_links_key JOIN files s ON s.id = l.src_file
                         WHERE l.key IN (?1, ?2) AND {DANGLING} AND {}",
                        shown_file("s", shown.images)
                    ))?;
                    let key = link_key(rel);
                    let rows = st.query_map([key.clone(), link_key(&strip_ext(rel))], |r| {
                        Ok((row(r)?, r.get::<_, String>(3)?))
                    })?;
                    for found in rows {
                        let (file, target) = found?;
                        if missing_path(&target).is_some_and(|m| link_key(&m) == key) {
                            b.file(file);
                        }
                    }
                }
            }
        }

        self.respell_ghosts(&mut b, shown.images)?;

        // Every edge among the nodes is a link, an import or a tag one of the files writes, read
        // for all of them at once and taken file by file in the nodes' order.
        let mut files: Vec<(i64, u32)> = b.files.iter().map(|(&id, &i)| (id, i)).collect();
        files.sort_unstable_by_key(|&(_, i)| i);
        let ids: Vec<i64> = files.iter().map(|&(id, _)| id).collect();
        let mut links = self.links_among(&ids)?;
        let mut imports = self.imports_out(&ids, shown.images)?;
        let mut tags = match b.tags.is_empty() {
            true => HashMap::new(),
            false => self.tags_of_files(&ids)?,
        };
        for (id, from) in files {
            for (kind, resolved, dangling) in links.remove(&id).unwrap_or_default() {
                let to = match (resolved, dangling) {
                    (Some(to), _) => b.files.get(&to),
                    (None, Some(target)) => {
                        missing_path(&target).and_then(|m| b.missing.get(&link_key(&m)))
                    }
                    (None, None) => None,
                };
                if let Some(&to) = to {
                    b.edge(from, to, edge_kind(kind));
                }
            }
            for ((to, ..), _) in imports.remove(&id).unwrap_or_default() {
                if let Some(&to) = b.files.get(&to) {
                    b.edge(from, to, EdgeKind::Import);
                }
            }
            for name in tags.remove(&id).unwrap_or_default() {
                if let Some(&to) = b.tags.get(&name) {
                    b.edge(from, to, EdgeKind::Tag);
                }
            }
        }
        Ok(b.part)
    }

    /// The vault's whole graph: every file [`shown_file`] takes, the links and imports among them,
    /// and the ghosts and tags `shown` asks for; without `shown.orphans`, only the nodes some edge
    /// joins.
    pub fn graph_all(&self, shown: Shown) -> Result<Part> {
        let mut b = Builder::default();
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT id, rel_path, doc FROM files f WHERE {}",
            shown_file("f", shown.images)
        ))?;
        for file in st.query_map([], row)? {
            b.file(file?);
        }
        let mut st = self.conn.prepare_cached(
            "SELECT src_file, resolved_file, kind FROM links
             WHERE kind <> 3 AND resolved_file IS NOT NULL AND resolved_file <> src_file",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        for link in rows {
            let (from, to, kind): (i64, i64, i64) = link?;
            if let (Some(&from), Some(&to)) = (b.files.get(&from), b.files.get(&to)) {
                b.edge(from, to, edge_kind(kind));
            }
        }
        // A Go file's import names a package, every Go file of its folder but the tests.
        let mut packages: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, node) in b.part.nodes.iter().enumerate() {
            if in_package(&node.key) {
                let dir = parent_dir(&node.key).to_string();
                packages.entry(dir).or_default().push(i as u32);
            }
        }
        let mut st = self.conn.prepare_cached(
            "SELECT i.src_file, i.resolved_file, t.rel_path FROM imports i
             JOIN files t ON t.id = i.resolved_file",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        for import in rows {
            let (from, to, rel): (i64, i64, String) = import?;
            let Some(&from) = b.files.get(&from) else {
                continue;
            };
            let to: Vec<u32> =
                match b.part.nodes[from as usize].key.ends_with(".go") && in_package(&rel) {
                    true => packages.get(parent_dir(&rel)).cloned().unwrap_or_default(),
                    false => b.files.get(&to).copied().into_iter().collect(),
                };
            for to in to {
                b.edge(from, to, EdgeKind::Import);
            }
        }
        if shown.unresolved {
            // In the order of their targets, as `missing_notes` lists them.
            let mut st = self.conn.prepare_cached(&format!(
                "SELECT l.src_file, l.target, l.kind FROM links l JOIN files s ON s.id = l.src_file
                 WHERE {DANGLING} ORDER BY l.target"
            ))?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            for link in rows {
                let (from, target, kind): (i64, String, i64) = link?;
                if let (Some(&from), Some(rel)) = (b.files.get(&from), missing_path(&target)) {
                    let to = b.missing(rel);
                    b.edge(from, to, edge_kind(kind));
                }
            }
        }
        if shown.tags {
            let mut st = self
                .conn
                .prepare_cached("SELECT DISTINCT file_id, name FROM tags")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            for tag in rows {
                let (from, name): (i64, String) = tag?;
                if let Some(&from) = b.files.get(&from) {
                    let to = b.tag(name);
                    b.edge(from, to, EdgeKind::Tag);
                }
            }
        }
        let mut part = b.part;
        if !shown.orphans {
            let mut joined = vec![false; part.nodes.len()];
            for e in &part.edges {
                (joined[e.from as usize], joined[e.to as usize]) = (true, true);
            }
            part.retain(|i, _| joined[i]);
        }
        Ok(part)
    }

    /// Key each ghost of `b` by the least spelling of every link naming it, the ones the answer
    /// leaves out too, so it has the key the whole vault's graph gives it and keeps it as the
    /// graph grows. By [`DANGLING`]'s rule from the files [`shown_file`] takes, as `graph_all`
    /// meets them, and by both keys a link to it can have, as `Seed::Missing` looks them up.
    fn respell_ghosts(&self, b: &mut Builder, images: bool) -> Result<()> {
        if b.missing.is_empty() {
            return Ok(());
        }
        let mut keys = Vec::new();
        for (key, &i) in &b.missing {
            keys.push(key.clone());
            keys.push(link_key(&strip_ext(&b.part.nodes[i as usize].key)));
        }
        for keys in keys.chunks(BATCH) {
            let mut st = self.conn.prepare_cached(&format!(
                "SELECT l.target FROM links l INDEXED BY idx_links_key
                 JOIN files s ON s.id = l.src_file
                 WHERE l.key IN ({}) AND {DANGLING} AND {}",
                parameters(keys.len()),
                shown_file("s", images)
            ))?;
            let targets = st.query_map(rusqlite::params_from_iter(keys), |r| r.get(0))?;
            for target in targets {
                let target: String = target?;
                if let Some(rel) = missing_path(&target)
                    && let Some(&i) = b.missing.get(&link_key(&rel))
                {
                    b.respell(i, rel);
                }
            }
        }
        Ok(())
    }

    /// The links file `id` writes but external ones, each with where it leads.
    fn links_out(&self, id: i64, images: bool) -> Result<Vec<Out>> {
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT l.target, f.id, f.rel_path, f.doc, f.id IS NOT NULL AND {}, {DANGLING}
             FROM links l JOIN files s ON s.id = l.src_file LEFT JOIN files f ON f.id = l.resolved_file
             WHERE l.src_file = ?1 AND l.kind <> 3",
            shown_file("f", images)
        ))?;
        let rows = st.query_map([id], |r| {
            let file = match r.get::<_, Option<i64>>(1)? {
                Some(to) => Some((to, r.get(2)?, r.get(3)?)),
                None => None,
            };
            Ok(Out {
                target: r.get(0)?,
                file,
                shown: r.get(4)?,
                dangling: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The links the files `ids` write but external ones as an edge needs them ([`Link`]), by
    /// file in the order it wrote them: without [`Index::links_out`]'s look at the file each
    /// resolved to, in a third of the time.
    fn links_among(&self, ids: &[i64]) -> Result<HashMap<i64, Vec<Link>>> {
        let select = |ids: &str| {
            format!(
                "SELECT l.src_file, l.kind, l.resolved_file, CASE WHEN {DANGLING} THEN l.target END
                 FROM links l JOIN files s ON s.id = l.src_file
                 WHERE l.src_file IN ({ids}) AND l.kind <> 3 ORDER BY l.src_file, l.rowid"
            )
        };
        self.per_file(select, ids, |r| Ok((r.get(1)?, r.get(2)?, r.get(3)?)))
    }

    /// The files the files `ids` import, by file, each with whether [`shown_file`] takes it: the
    /// file each import resolved to, or where a Go file imports a package, every file
    /// [`in_package`] of the folder that file is in.
    fn imports_out(&self, ids: &[i64], images: bool) -> Result<HashMap<i64, Vec<(Row, bool)>>> {
        let select = |ids: &str| {
            format!(
                "SELECT i.src_file, t.id, t.rel_path, t.doc, {}, s.rel_path FROM imports i
                 JOIN files s ON s.id = i.src_file JOIN files t ON t.id = i.resolved_file
                 WHERE i.src_file IN ({ids}) ORDER BY i.src_file, i.rowid",
                shown_file("t", images)
            )
        };
        let rows = self.per_file(select, ids, |r| {
            let file: Row = (r.get(1)?, r.get(2)?, r.get(3)?);
            Ok((file, r.get::<_, bool>(4)?, r.get::<_, String>(5)?))
        })?;
        let mut packages: HashMap<String, Vec<(Row, bool)>> = HashMap::new();
        let mut out = HashMap::new();
        for (id, rows) in rows {
            let mut imported = Vec::new();
            for (file, taken, src) in rows {
                match src.ends_with(".go") && in_package(&file.1) {
                    true => {
                        let dir = parent_dir(&file.1);
                        if !packages.contains_key(dir) {
                            packages.insert(dir.to_string(), self.package(dir, images)?);
                        }
                        imported.extend(packages[dir].iter().cloned());
                    }
                    false => imported.push((file, taken)),
                }
            }
            out.insert(id, imported);
        }
        Ok(out)
    }

    /// The files [`in_package`] of the folder `dir`, each with whether [`shown_file`] takes it.
    fn package(&self, dir: &str, images: bool) -> Result<Vec<(Row, bool)>> {
        let mut st = self.conn.prepare_cached(&format!(
            "SELECT f.id, f.rel_path, f.doc, {} FROM files f
             WHERE f.parent_dir = ?1 AND f.rel_path LIKE '%.go'",
            shown_file("f", images)
        ))?;
        let rows = st.query_map([dir], |r| Ok((row(r)?, r.get(3)?)))?;
        let rows = rows.collect::<rusqlite::Result<Vec<(Row, bool)>>>()?;
        Ok(rows
            .into_iter()
            .filter(|(file, _)| in_package(&file.1))
            .collect())
    }

    /// The files importing file `rel` that [`shown_file`] takes: for a Go file, those whose
    /// import of its package resolved to any file of it.
    fn imports_in(&self, id: i64, rel: &str, images: bool) -> Result<Vec<Row>> {
        let filter = shown_file("s", images);
        let rows = match in_package(rel) {
            true => {
                let mut st = self.conn.prepare_cached(&format!(
                    "SELECT DISTINCT s.id, s.rel_path, s.doc FROM files t
                     JOIN imports i ON i.resolved_file = t.id JOIN files s ON s.id = i.src_file
                     WHERE t.parent_dir = ?1 AND t.rel_path LIKE '%.go' AND s.rel_path LIKE '%.go'
                       AND {filter}"
                ))?;
                st.query_map([parent_dir(rel)], row)?
                    .collect::<rusqlite::Result<_>>()?
            }
            false => {
                let mut st = self.conn.prepare_cached(&format!(
                    "SELECT DISTINCT s.id, s.rel_path, s.doc FROM imports i
                     JOIN files s ON s.id = i.src_file WHERE i.resolved_file = ?1 AND {filter}"
                ))?;
                st.query_map([id], row)?.collect::<rusqlite::Result<_>>()?
            }
        };
        Ok(rows)
    }

    /// The tags the files `ids` carry, by file, each once in the order it first carries it.
    fn tags_of_files(&self, ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
        let select = |ids: &str| {
            format!(
                "SELECT file_id, name FROM tags WHERE file_id IN ({ids})
                 GROUP BY file_id, name ORDER BY file_id, MIN(rowid)"
            )
        };
        self.per_file(select, ids, |r| r.get(1))
    }

    /// What `select` reads of the files `ids`, gathered by file in the order it came: `select`
    /// is handed the parameters of its `IN (…)`, which takes [`BATCH`] of the files at a time,
    /// and its first column names the file.
    fn per_file<T>(
        &self,
        select: impl Fn(&str) -> String,
        ids: &[i64],
        read: impl Fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<HashMap<i64, Vec<T>>> {
        let mut out: HashMap<i64, Vec<T>> = HashMap::new();
        for ids in ids.chunks(BATCH) {
            let mut st = self.conn.prepare_cached(&select(&parameters(ids.len())))?;
            let rows = st.query_map(rusqlite::params_from_iter(ids), |r| {
                Ok((r.get::<_, i64>(0)?, read(r)?))
            })?;
            for found in rows {
                let (id, row) = found?;
                out.entry(id).or_default().push(row);
            }
        }
        Ok(out)
    }
}

/// The most values one `IN (…)` is handed, well under SQLite's 32766 parameters.
const BATCH: usize = 500;

/// `n` parameters, for an `IN (…)`.
fn parameters(n: usize) -> String {
    vec!["?"; n].join(", ")
}

#[cfg(test)]
mod tests {
    use crate::graph::{EdgeKind, NodeKind, Part, Shown};
    use crate::index::testing::{fixture, open};
    use crate::query::Doc;
    use std::fs;

    fn keys(part: &Part) -> Vec<&str> {
        let mut keys: Vec<&str> = part.nodes.iter().map(|n| n.key.as_str()).collect();
        keys.sort();
        keys
    }

    fn edges(part: &Part) -> Vec<(&str, &str, EdgeKind)> {
        let key = |i: u32| part.nodes[i as usize].key.as_str();
        let mut edges: Vec<_> = part
            .edges
            .iter()
            .map(|e| (key(e.from), key(e.to), e.kind))
            .collect();
        edges.sort_by_key(|&(from, to, _)| (from, to));
        edges
    }

    fn all(unresolved: bool, tags: bool, images: bool, orphans: bool) -> Shown {
        Shown {
            unresolved,
            tags,
            images,
            orphans,
        }
    }

    /// The fixture's `[[Beta]]` and `#rust`, and a note embedding a picture, linking to a note
    /// that is not there, and one note nothing links.
    #[test]
    fn graph_all_holds_links_embeds_ghosts_and_tags_as_shown() {
        let (vault, db) = fixture();
        fs::write(
            vault.path().join("d.md"),
            "[[a]] ![[pic.png]] [[Gone]] #rust\n",
        )
        .unwrap();
        fs::write(vault.path().join("pic.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        fs::write(vault.path().join("e.md"), "alone\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(vault.path(), |_| {}).unwrap();

        let part = ix.graph_all(Shown::default()).unwrap();
        assert_eq!(keys(&part), ["Gone.md", "a.md", "d.md", "sub/Beta.md"]);
        use EdgeKind::*;
        assert_eq!(
            edges(&part),
            [
                ("a.md", "sub/Beta.md", Link),
                ("d.md", "Gone.md", Link),
                ("d.md", "a.md", Link)
            ]
        );
        let kind = |key: &str| part.nodes.iter().find(|n| n.key == key).unwrap().kind;
        assert_eq!(kind("a.md"), NodeKind::File(Some(Doc::Note)));
        assert_eq!(kind("Gone.md"), NodeKind::Missing);

        let part = ix.graph_all(all(false, true, true, false)).unwrap();
        assert_eq!(
            keys(&part),
            ["#rust", "a.md", "d.md", "pic.png", "sub/Beta.md"]
        );
        assert!(edges(&part).contains(&("d.md", "pic.png", Embed)));
        assert!(edges(&part).contains(&("a.md", "#rust", Tag)));

        // Orphans, but never the folder or the conflict copy.
        let part = ix.graph_all(all(false, false, false, true)).unwrap();
        assert_eq!(
            keys(&part),
            ["a.md", "c.pdf", "d.md", "e.md", "sub/Beta.md"]
        );
    }

    /// A Graph tab's answer: the keys, their neighbours either way, and every edge among them,
    /// whether a key is a file, a second path to one, a tag or a missing note.
    #[test]
    fn graph_around_answers_the_keys_their_neighbours_and_every_edge_among_them() {
        let (vault, db) = fixture();
        let root = vault.path();
        fs::write(root.join("d.md"), "[[a]] [[Beta]] [[Gone]] #rust\n").unwrap();
        std::os::unix::fs::symlink("a.md", root.join("alpha.md")).unwrap();
        let mut ix = open(&db);
        ix.reconcile(root, |_| {}).unwrap();
        let around = |keys: &[&str], shown| {
            let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
            ix.graph_around(&keys, shown).unwrap()
        };
        use EdgeKind::*;

        // `d.md` → `sub/Beta.md` is no edge of `a.md`'s, but both ends are in the answer.
        let part = around(&["a.md"], Shown::default());
        assert_eq!(keys(&part), ["a.md", "d.md", "sub/Beta.md"]);
        assert_eq!(
            edges(&part),
            [
                ("a.md", "sub/Beta.md", Link),
                ("d.md", "a.md", Link),
                ("d.md", "sub/Beta.md", Link)
            ]
        );
        assert_eq!(around(&["alpha.md"], Shown::default()), part);

        let part = around(&["#rust"], all(false, false, false, false));
        assert_eq!(keys(&part), ["#rust", "a.md", "d.md"]);
        assert!(edges(&part).contains(&("a.md", "#rust", Tag)));
        assert!(edges(&part).contains(&("d.md", "a.md", Link)));

        let part = around(&["Gone.md"], all(false, false, false, false));
        assert_eq!(keys(&part), ["Gone.md", "d.md"]);
        assert_eq!(edges(&part), [("d.md", "Gone.md", Link)]);
    }

    /// A missing note linked in two spellings is one ghost, keyed by the least of them whichever
    /// note links it: in the whole vault's graph and round either note or both.
    #[test]
    fn a_ghost_is_keyed_by_its_least_spelling() {
        let (vault, db) = fixture();
        let root = vault.path();
        fs::write(root.join("x.md"), "[[gone]]\n").unwrap();
        fs::write(root.join("y.md"), "[[Gone]]\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(root, |_| {}).unwrap();
        let ghosts = |part: Part| {
            let ghosts = part
                .nodes
                .into_iter()
                .filter(|n| n.kind == NodeKind::Missing);
            ghosts.map(|n| n.key).collect::<Vec<_>>()
        };
        assert_eq!(ghosts(ix.graph_all(Shown::default()).unwrap()), ["Gone.md"]);
        for keys in [&["x.md"][..], &["y.md"], &["x.md", "y.md"]] {
            let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
            let part = ix.graph_around(&keys, Shown::default()).unwrap();
            assert_eq!(ghosts(part), ["Gone.md"], "round {keys:?}");
        }
        // A missing note handed in keeps the spelling it was handed in.
        let part = ix
            .graph_around(&["gone.md".into()], Shown::default())
            .unwrap();
        assert_eq!(ghosts(part), ["gone.md"]);
    }

    /// A note in a gitignored folder that a tab opened is no neighbour of what it links to, and
    /// misses no note, but is its own graph's middle when it is in front.
    #[test]
    fn an_opened_ignored_note_is_left_out_but_kept_as_a_key() {
        let (vault, db) = fixture();
        let root = vault.path();
        fs::write(root.join(".gitignore"), "build/\n").unwrap();
        fs::create_dir(root.join("build")).unwrap();
        fs::write(root.join("build/out.md"), "[[a]] [[Nowhere]]\n").unwrap();
        let mut ix = open(&db);
        ix.reconcile(root, |_| {}).unwrap();
        ix.keep_opened(root, "build/out.md").unwrap();

        let a = ix.graph_around(&["a.md".into()], Shown::default()).unwrap();
        assert_eq!(keys(&a), ["a.md", "sub/Beta.md"]);
        let out = ix
            .graph_around(&["build/out.md".into()], Shown::default())
            .unwrap();
        assert_eq!(keys(&out), ["a.md", "build/out.md"]);
        assert_eq!(edges(&out), [("build/out.md", "a.md", EdgeKind::Link)]);
        assert!(
            !keys(&ix.graph_all(all(true, false, false, true)).unwrap()).contains(&"build/out.md")
        );
    }

    /// A code file's imports are edges, and a Go file's import of a package one to each of its
    /// files but the tests.
    #[test]
    fn imports_are_edges_and_a_go_import_reaches_its_whole_package() {
        let (vault, db) = fixture();
        let root = vault.path();
        fs::create_dir_all(root.join("app")).unwrap();
        fs::create_dir_all(root.join("proj/pkg")).unwrap();
        fs::write(root.join("app/main.py"), "import utils\n").unwrap();
        fs::write(root.join("app/utils.py"), "").unwrap();
        let main = "package main\n\nimport \"example.com/me/proj/pkg\"\n";
        fs::write(root.join("proj/main.go"), main).unwrap();
        for file in ["a.go", "b.go", "a_test.go"] {
            fs::write(root.join("proj/pkg").join(file), "package pkg\n").unwrap();
        }
        let mut ix = open(&db);
        ix.reconcile(root, |_| {}).unwrap();
        use EdgeKind::Import;

        let part = ix.graph_all(Shown::default()).unwrap();
        for edge in [
            ("app/main.py", "app/utils.py", Import),
            ("proj/main.go", "proj/pkg/a.go", Import),
            ("proj/main.go", "proj/pkg/b.go", Import),
        ] {
            assert!(
                edges(&part).contains(&edge),
                "{edge:?} in {:?}",
                edges(&part)
            );
        }
        assert!(!keys(&part).contains(&"proj/pkg/a_test.go"));

        let part = ix
            .graph_around(&["proj/pkg/b.go".into()], Shown::default())
            .unwrap();
        assert_eq!(keys(&part), ["proj/main.go", "proj/pkg/b.go"]);
        assert_eq!(edges(&part), [("proj/main.go", "proj/pkg/b.go", Import)]);
        let part = ix
            .graph_around(&["app/utils.py".into()], Shown::default())
            .unwrap();
        assert_eq!(edges(&part), [("app/main.py", "app/utils.py", Import)]);
    }

    /// `ACCENT_GRAPH_VAULT=/tmp/<scratch> cargo test --release -p accent-core -- --ignored
    /// graph_all_cost --nocapture`: the whole graph of a generated vault, timed, and the size of
    /// its JSON, which is what a remote vault sends.
    #[test]
    #[ignore = "needs ACCENT_GRAPH_VAULT, a generated vault (never the real one)"]
    fn graph_all_cost() {
        let vault = std::env::var_os("ACCENT_GRAPH_VAULT").expect("ACCENT_GRAPH_VAULT");
        let db = tempfile::tempdir().unwrap();
        let mut ix = open(&db);
        ix.reconcile(std::path::Path::new(&vault), |_| {}).unwrap();
        for shown in [Shown::default(), all(true, true, true, true)] {
            let started = std::time::Instant::now();
            let part = ix.graph_all(shown).unwrap();
            let took = started.elapsed();
            let json = serde_json::to_vec(&part).unwrap().len();
            println!(
                "graph_all {shown:?}: {} nodes, {} edges in {took:?}, {json} bytes of JSON",
                part.nodes.len(),
                part.edges.len()
            );
        }
    }
}
