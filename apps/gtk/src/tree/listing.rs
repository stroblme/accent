//! Asynchronous directory listings encoded as strings and spliced into
//! cached stores, preserving expandable row identity.

use super::*;

/// Encode rows as `GtkStringObject`s containing `"<kind><state><rel_path>"`.
///
/// This avoids a custom GObject subclass for the current small state. `kind` is `d` or `f`;
/// `state` is indexed `i`, gitignored `g` or dependency-tree `x` ([`Row::dependency`]).
fn encode(kind: FileKind, rel: &str, indexed: bool, dependency: bool) -> String {
    // What a file is — its icon, what it opens as — is read off its name, so a directory is the
    // one thing the row has to carry.
    let c = match kind {
        FileKind::Dir => 'd',
        _ => 'f',
    };
    let state = match (indexed, dependency) {
        (true, _) => 'i',
        (false, false) => 'g',
        (false, true) => 'x',
    };
    format!("{c}{state}{rel}")
}

pub fn decode(item: &glib::Object) -> Option<Row> {
    decode_str(&item.downcast_ref::<gtk::StringObject>()?.string())
}

/// Pure row decoding for testing the encoding without a GTK window.
fn decode_str(s: &str) -> Option<Row> {
    let mut cs = s.chars();
    let kind = cs.next()?;
    let state = cs.next()?;
    Some(Row {
        kind,
        rel: cs.as_str().to_string(),
        indexed: state == 'i',
        dependency: state == 'x',
    })
}

/// Hide Syncthing conflicts and indexed dot names when `show_hidden` is off.
///
/// Keep unindexed skipped trees visible, including `.venv`; hiding them would conceal folders
/// the tree deliberately lists. `.git` and `.trash` are excluded upstream regardless.
pub fn hidden(row_kind: FileKind, rel: &str, indexed: bool, show_hidden: bool) -> bool {
    indexed
        && (row_kind == FileKind::Conflict
            || (!show_hidden && dot_named(rel))
            || rel.rsplit('/').next().is_some_and(is_sync_conflict))
}

/// A dot-named path, or one inside a dot-named folder: what a file manager calls hidden.
pub fn dot_named(rel: &str) -> bool {
    rel.split('/').any(|c| c.starts_with('.'))
}

/// Per-directory request counters, with `""` naming the root.
///
/// Concurrent listings can return out of order over a link. Apply only the newest so stale
/// responses cannot restore removed rows.
pub(super) type Asked = Rc<RefCell<HashMap<String, u64>>>;

/// What shapes a listing: Show Hidden Files, read when a response lands so a listing in flight
/// honours a toggle made after the request, and the order the vault asks for.
#[derive(Clone, Copy)]
pub(super) struct Shape {
    pub show_hidden: bool,
    pub order: Order,
}

pub(super) type Shaped = Rc<Cell<Shape>>;

/// Run when a listing of the root lands, whether or not it changed the store.
pub(super) type Landed = Option<Rc<dyn Fn()>>;

/// Update `store` with direct children of `prefix` on a worker, then call `landed`.
///
/// The store may remain empty briefly so remote expansion never blocks on a round trip.
/// `list_dir_sorted` sorts where the files are, in the order `shape` holds when it is asked.
/// `model` is the tree the store is listed in, whose folders the refill keeps open; `None` for a
/// first listing, which has none.
pub(super) fn fill(
    store: &gio::ListStore,
    vault: &Arc<Vault>,
    asked: &Asked,
    shape: &Shaped,
    prefix: &str,
    landed: Landed,
    model: Option<&gtk::TreeListModel>,
) {
    let ticket = {
        let mut asked = asked.borrow_mut();
        let n = asked.entry(prefix.to_string()).or_default();
        *n += 1;
        *n
    };
    let (store, vault, asked, shape) = (store.clone(), vault.clone(), asked.clone(), shape.clone());
    let (dir, order, model) = (prefix.to_string(), shape.get().order, model.cloned());
    glib::spawn_future_local(async move {
        let listed = crate::work::off_thread("tree", {
            let dir = dir.clone();
            move || vault.list_dir_sorted(&dir, order)
        })
        .await;
        // A newer listing of this directory was asked for while this one was on its way.
        if asked.borrow().get(&dir) != Some(&ticket) {
            return;
        }
        match listed {
            Some(Ok(rows)) => {
                splice(&store, rows, shape.get().show_hidden, model.as_ref(), &dir);
                if let Some(landed) = landed {
                    landed();
                }
            }
            // Leaving the rows alone beats blanking a directory the index simply could not answer
            // for — or, on a remote vault, one the connection could not reach.
            Some(Err(e)) => tracing::warn!("listing a directory failed: {e:#}"),
            None => {}
        }
    });
}

/// The rows the listing of `dir` produced, against the ones the store already holds.
fn splice(
    store: &gio::ListStore,
    rows: Vec<accent_api::FileRow>,
    show_hidden: bool,
    model: Option<&gtk::TreeListModel>,
    dir: &str,
) {
    let items: Vec<String> = rows
        .into_iter()
        // `id == 0` is `Vault::list_dir` saying this row came off the disk rather than out of
        // the index.
        .filter(|r| !hidden(r.kind, &r.rel_path, r.id != 0, show_hidden))
        .map(|r| encode(r.kind, &r.rel_path, r.id != 0, r.dependency))
        .collect();
    let Some((at, removed, added)) = changed_span(&current(store), &items) else {
        return;
    };
    let new: Vec<gtk::StringObject> = items[at..at + added]
        .iter()
        .map(|s| gtk::StringObject::new(s))
        .collect();
    // A row taken out and put back is a new row, shut: a sort, or a folder a time sort moves,
    // would shut every folder open in the span.
    let open = model
        .filter(|_| removed > 0)
        .map(|model| open_in(model, dir, at, removed));
    // One splice, one `items-changed`. Appending row by row made a 2 400-child directory emit
    // 2 400 signals out through TreeListModel -> SingleSelection -> ListView.
    store.splice(at as u32, removed as u32, &new);
    if let (Some(model), Some(open)) = (model, open) {
        for rel in open {
            if let Some(row) = super::find_row(model, &rel) {
                row.set_expanded(true);
            }
        }
    }
}

/// The folders open at or under rows `at..at + n` of `dir`'s listing in `model`, outermost first.
fn open_in(model: &gtk::TreeListModel, dir: &str, at: usize, n: usize) -> Vec<String> {
    let parent = match dir {
        "" => None,
        dir => match super::find_row(model, dir) {
            Some(row) => Some(row),
            // Shut, or not listed: nothing under it is open.
            None => return Vec::new(),
        },
    };
    let mut open = Vec::new();
    for i in at..at + n {
        let row = match &parent {
            Some(parent) => parent.child_row(i as u32),
            None => model.child_row(i as u32),
        };
        let Some(row) = row.filter(|row| row.is_expanded()) else {
            continue;
        };
        // The rows under it follow it in the model, deeper than it, in order.
        let depth = row.depth();
        for position in row.position().. {
            let Some(below) = model.item(position).and_downcast::<gtk::TreeListRow>() else {
                break;
            };
            if position > row.position() && below.depth() <= depth {
                break;
            }
            if below.is_expanded()
                && let Some(item) = below.item().as_ref().and_then(decode)
            {
                open.push(item.rel);
            }
        }
    }
    open
}

/// The encoded value of every row currently in `store`.
fn current(store: &gio::ListStore) -> Vec<String> {
    (0..store.n_items())
        .filter_map(|i| store.item(i).and_downcast::<gtk::StringObject>())
        .map(|s| s.string().to_string())
        .collect()
}

/// Return the differing span of `old` and `new` as start, removed rows and inserted rows.
///
/// Trim equal prefixes and suffixes to retain row objects, whose identity owns expanded
/// children, scroll state and a list's row widgets. Equal listings return `None`.
pub fn changed_span<T: PartialEq>(old: &[T], new: &[T]) -> Option<(usize, usize, usize)> {
    let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    match (old.len() - head - tail, new.len() - head - tail) {
        (0, 0) => None,
        (removed, added) => Some((head, removed, added)),
    }
}

/// Unindexed folders whose cached listings the vault watches for changes.
pub(super) type Watches = Rc<RefCell<HashSet<String>>>;

/// Watch unindexed `dir` while the tree retains its listing.
///
/// Gitignored folders need one-level watches to show external build output without reopening.
/// Child models and watches last with the window, even when collapsed. The vault watches
/// where files live, including remote hosts; debounced
/// [`Event::UnindexedChanged`](accent_api::Event::UnindexedChanged) refreshes listings.
/// Dependency trees such as node_modules remain unwatched to avoid huge watch sets.
pub(super) fn watch_unindexed(watches: &Watches, vault: &Arc<Vault>, dir: &str) {
    if !watches.borrow_mut().insert(dir.to_string()) {
        return;
    }
    // A round trip on a remote vault, from a row being bound: sent from a worker and not waited
    // for. One that cannot be sent yet is kept, and asked of the host once it answers.
    let (vault, dirs) = (vault.clone(), vec![dir.to_string()]);
    gio::spawn_blocking(move || {
        if let Err(e) = vault.watch_unindexed(&dirs) {
            tracing::debug!("watching an unindexed folder: {e:#}");
        }
    });
}

pub(super) fn children_model(
    vault: &Arc<Vault>,
    cache: &Rc<RefCell<HashMap<String, gio::ListStore>>>,
    asked: &Asked,
    shape: &Shaped,
    rel: &str,
) -> gio::ListStore {
    // Cloned out so the cache borrow cannot still be live during `fill`.
    let hit = cache.borrow().get(rel).cloned();
    if let Some(store) = hit {
        return store;
    }
    let t0 = Instant::now();
    let store = gio::ListStore::new::<gtk::StringObject>();
    fill(&store, vault, asked, shape, rel, None, None);
    cache.borrow_mut().insert(rel.to_string(), store.clone());
    tracing::debug!(
        dir = rel,
        rows = store.n_items(),
        ms = t0.elapsed().as_secs_f64() * 1e3,
        "expanded directory"
    );
    store
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_row_carries_whether_the_index_holds_it() {
        let row = |kind, rel, indexed, dep| decode_str(&encode(kind, rel, indexed, dep)).unwrap();
        let note = row(FileKind::Markdown, "Notes/A.md", true, false);
        assert_eq!(note.kind, 'f');
        assert_eq!(note.rel, "Notes/A.md");
        assert!(note.indexed);
        assert!(!note.dependency);
        // A row read off the disk keeps its kind — the icon and the expander must not change —
        // and says the index has never heard of it.
        let dep = row(FileKind::Dir, "node_modules", false, true);
        assert_eq!(dep.kind, 'd');
        assert!(dep.is_dir());
        assert_eq!(dep.rel, "node_modules");
        assert!(!dep.indexed);
        assert!(dep.dependency);
        // A gitignored folder is out of the index too, and is still the reader's own.
        let ignored = row(FileKind::Dir, "mlruns", false, false);
        assert!(!ignored.indexed);
        assert!(!ignored.dependency);
    }

    #[test]
    fn show_hidden_decides_the_dot_named_rows_the_index_holds() {
        use FileKind::{Conflict, Dir, Markdown, Other};
        // Shown with the toggle on, left out with it off.
        for rel in [".gitignore", ".obsidian/app.json", "Notes/.draft.md"] {
            assert!(!hidden(Other, rel, true, true), "{rel}");
            assert!(hidden(Other, rel, true, false), "{rel}");
        }
        assert!(!hidden(Markdown, "Notes/a.md", true, false));
        // A dot-named tree the walk refuses is listed off the disk either way, as it was before
        // the toggle existed.
        assert!(!hidden(Dir, ".venv", false, false));
        assert!(!hidden(Other, ".venv/pyvenv.cfg", false, false));
        // A conflict copy never is: resolving one is the conflict banner's business.
        let conflict = "a.sync-conflict-20260903-101500-ABCDEFG.md";
        assert!(hidden(Conflict, conflict, true, true));
    }

    #[test]
    fn changed_span_reports_nothing_when_the_listing_is_unchanged() {
        let same = rows(&["dNotes", "ma.md", "mb.md"]);
        assert_eq!(changed_span(&same, &same), None);
        assert_eq!(changed_span::<String>(&[], &[]), None);
    }

    #[test]
    fn changed_span_covers_only_the_rows_that_moved() {
        let old = rows(&["dNotes", "ma.md", "mc.md"]);
        // Inserted in the middle: one row added, none removed.
        assert_eq!(
            changed_span(&old, &rows(&["dNotes", "ma.md", "mb.md", "mc.md"])),
            Some((2, 0, 1))
        );
        // Removed from the middle.
        assert_eq!(
            changed_span(&old, &rows(&["dNotes", "mc.md"])),
            Some((1, 1, 0))
        );
        // Renamed in place.
        assert_eq!(
            changed_span(&old, &rows(&["dNotes", "ma.md", "mz.md"])),
            Some((2, 1, 1))
        );
        // Appended at the end, so the head is everything that was already there.
        assert_eq!(
            changed_span(&old, &rows(&["dNotes", "ma.md", "mc.md", "md.md"])),
            Some((3, 0, 1))
        );
    }

    #[test]
    fn changed_span_handles_an_empty_side() {
        let listing = rows(&["dNotes", "ma.md"]);
        assert_eq!(changed_span(&[], &listing), Some((0, 0, 2)));
        assert_eq!(changed_span(&listing, &[]), Some((0, 2, 0)));
    }

    #[test]
    fn changed_span_keeps_a_repeated_row_from_widening_the_span() {
        // Equal head and tail must not overlap, or the span would remove more than there is.
        let old = rows(&["ma.md", "ma.md"]);
        assert_eq!(changed_span(&old, &rows(&["ma.md"])), Some((1, 1, 0)));
        assert_eq!(changed_span(&rows(&["ma.md"]), &old), Some((1, 0, 1)));
    }
}
