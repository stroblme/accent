//! The order the file tree lists a folder in (user decision 2026-11-09): Obsidian's six sorts,
//! kept per vault, with folders on top unless a preference sorts them among the files.

use crate::index::FileRow;
use crate::walk::FileKind;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// What a folder's rows are sorted by, and which way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Sort {
    /// By name, A to Z, case aside: the order the tree has always had.
    #[default]
    NameAsc,
    NameDesc,
    /// Newest first.
    ModifiedNew,
    ModifiedOld,
    CreatedNew,
    CreatedOld,
}

impl Sort {
    pub const ALL: [Sort; 6] = [
        Sort::NameAsc,
        Sort::NameDesc,
        Sort::ModifiedNew,
        Sort::ModifiedOld,
        Sort::CreatedNew,
        Sort::CreatedOld,
    ];

    /// The name a session and a menu's action target spell it with.
    pub fn name(self) -> &'static str {
        match self {
            Sort::NameAsc => "name-asc",
            Sort::NameDesc => "name-desc",
            Sort::ModifiedNew => "modified-new",
            Sort::ModifiedOld => "modified-old",
            Sort::CreatedNew => "created-new",
            Sort::CreatedOld => "created-old",
        }
    }

    pub fn from_name(name: &str) -> Option<Sort> {
        Sort::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Whether it sorts by a time, which is read off the disk for each row.
    fn timed(self) -> bool {
        !matches!(self, Sort::NameAsc | Sort::NameDesc)
    }
}

/// How the tree orders one folder's rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub sort: Sort,
    /// Folders are sorted among the files by the same key; otherwise they stay on top, by name,
    /// Z to A only when the files are.
    pub folders_among_files: bool,
}

/// What a row is compared by.
struct Key {
    dir: bool,
    name: String,
    time: i64,
}

impl Order {
    /// Sort `rows`, the listing of the folder at `root`'s `rel` paths, reading each row's time
    /// off the disk for a time sort.
    pub fn sort(self, root: &Path, rows: &mut Vec<FileRow>) {
        let mut keyed: Vec<(Key, FileRow)> = rows
            .drain(..)
            .map(|row| {
                let key = Key {
                    dir: row.kind == FileKind::Dir,
                    // ASCII folding, as the index's `COLLATE NOCASE` and the merged listing do.
                    name: row.rel_path.to_ascii_lowercase(),
                    time: match self.sort.timed() {
                        true => time(&root.join(&row.rel_path), self.sort),
                        false => 0,
                    },
                };
                (key, row)
            })
            .collect();
        keyed.sort_by(|(a, _), (b, _)| self.compare(a, b));
        rows.extend(keyed.into_iter().map(|(_, row)| row));
    }

    fn compare(self, a: &Key, b: &Key) -> Ordering {
        let on_top = !self.folders_among_files;
        let name = a.name.cmp(&b.name);
        if on_top && a.dir != b.dir {
            return b.dir.cmp(&a.dir);
        }
        if on_top && a.dir {
            return match self.sort {
                Sort::NameDesc => name.reverse(),
                _ => name,
            };
        }
        // A time ties by name, A to Z either way.
        match self.sort {
            Sort::NameAsc => name,
            Sort::NameDesc => name.reverse(),
            Sort::ModifiedNew | Sort::CreatedNew => b.time.cmp(&a.time).then(name),
            Sort::ModifiedOld | Sort::CreatedOld => a.time.cmp(&b.time).then(name),
        }
    }
}

/// `path`'s modification or creation time in nanoseconds, a link followed; 0 for none. A
/// filesystem or a libc that keeps no creation time (`statx` without a birth time, a musl build)
/// gives the modification time instead.
fn time(path: &Path, sort: Sort) -> i64 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    let modified = meta.mtime() * 1_000_000_000 + meta.mtime_nsec();
    if !matches!(sort, Sort::CreatedNew | Sort::CreatedOld) {
        return modified;
    }
    meta.created()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(modified, |at| at.as_nanos() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(order: Order, rows: &[(&str, bool, i64)]) -> Vec<String> {
        let mut keys: Vec<Key> = rows
            .iter()
            .map(|&(name, dir, time)| Key {
                dir,
                name: name.to_ascii_lowercase(),
                time,
            })
            .collect();
        keys.sort_by(|a, b| order.compare(a, b));
        keys.into_iter().map(|k| k.name).collect()
    }

    #[test]
    fn each_sort_orders_files_and_keeps_folders_on_top_by_name() {
        // Two folders and three files, the times running against the names.
        let rows = [
            ("b", true, 1),
            ("a", true, 2),
            ("x.md", false, 3),
            ("y.md", false, 5),
            ("z.md", false, 4),
        ];
        let on_top = |sort| {
            sorted(
                Order {
                    sort,
                    ..Order::default()
                },
                &rows,
            )
        };
        assert_eq!(on_top(Sort::NameAsc), ["a", "b", "x.md", "y.md", "z.md"]);
        // Folders follow the files only in a name sort.
        assert_eq!(on_top(Sort::NameDesc), ["b", "a", "z.md", "y.md", "x.md"]);
        assert_eq!(
            on_top(Sort::ModifiedNew),
            ["a", "b", "y.md", "z.md", "x.md"]
        );
        assert_eq!(on_top(Sort::CreatedOld), ["a", "b", "x.md", "z.md", "y.md"]);
        let among = |sort| {
            let order = Order {
                sort,
                folders_among_files: true,
            };
            sorted(order, &rows)
        };
        assert_eq!(among(Sort::NameDesc), ["z.md", "y.md", "x.md", "b", "a"]);
        assert_eq!(among(Sort::ModifiedNew), ["y.md", "z.md", "x.md", "a", "b"]);
    }

    #[test]
    fn a_time_tie_falls_back_to_the_name_and_case_is_folded() {
        let rows = [("B.md", false, 7), ("a.md", false, 7), ("c.md", false, 9)];
        let order = |sort| Order {
            sort,
            ..Order::default()
        };
        assert_eq!(
            sorted(order(Sort::ModifiedOld), &rows),
            ["a.md", "b.md", "c.md"]
        );
        assert_eq!(
            sorted(order(Sort::ModifiedNew), &rows),
            ["c.md", "a.md", "b.md"]
        );
    }

    #[test]
    fn a_sort_round_trips_its_name() {
        for sort in Sort::ALL {
            assert_eq!(Sort::from_name(sort.name()), Some(sort));
        }
        assert_eq!(Sort::from_name("size"), None);
    }
}
