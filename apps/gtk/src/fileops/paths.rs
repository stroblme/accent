//! Arithmetic on the paths the dialogs type and the tree drags: what a typed name resolves to,
//! where a drop may land, and what to call the move afterwards. Pure, and tested as such.

use accent_core::path::{basename, parent_dir};
use accent_core::walk::out_of_reach;

/// Every level of `dir`, outermost first: `a/b/c` yields `a`, `a/b`, `a/b/c`. What a `mkdir -p`
/// would have had to make, so a failure can say how far it got.
pub(super) fn levels(dir: &str) -> impl Iterator<Item = &str> {
    dir.match_indices('/')
        .map(|(at, _)| &dir[..at])
        .chain(std::iter::once(dir))
}

/// Whether the name already carries a markdown extension.
pub(super) fn is_markdown(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
    })
}

/// Split a file name into stem and extension, dot included. A leading dot belongs to the name.
pub(super) fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

/// The part of a name Rename selects, so typing replaces it: a file's stem, keeping the
/// extension, and a folder's whole name, since the dot in `Archive.2024` starts no extension.
pub(super) fn renamed_part(name: &str, is_dir: bool) -> &str {
    match is_dir {
        true => name,
        false => split_ext(name).0,
    }
}

/// `name` inside `dir`, where "" is the vault root.
pub(super) fn child_path(dir: &str, name: &str) -> String {
    match dir.trim_end_matches('/') {
        "" => name.to_string(),
        dir => format!("{dir}/{name}"),
    }
}

/// Where a paste of `name` lands in `dir`, given what is already there.
///
/// The name itself while nothing holds it, and otherwise GNOME's own mark, carried by the stem so
/// the extension still says what the file is: `notes.md`, `notes (copy).md`, `notes (copy 2).md`.
/// A folder takes it at the end — the dot in `Archive.2024` starts no extension, which is the rule
/// Rename's selection already follows.
///
/// Takes the lookup rather than the vault, so the arithmetic is testable and so the caller can
/// keep every `stat` it costs on the worker thread: each one is a round trip on a remote vault.
pub(super) fn free_path(
    dir: &str,
    name: &str,
    is_dir: bool,
    taken: impl Fn(&str) -> bool,
) -> String {
    let first = child_path(dir, name);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = match is_dir {
        true => (name, ""),
        false => split_ext(name),
    };
    // Counts up until a name is free, which it must be: the folder holds finitely many.
    for n in 1.. {
        let candidate = match n {
            1 => child_path(dir, &format!("{stem} (copy){ext}")),
            n => child_path(dir, &format!("{stem} (copy {n}){ext}")),
        };
        if !taken(&candidate) {
            return candidate;
        }
    }
    unreachable!("a folder cannot hold every name")
}

/// `rel` moved into `dest_dir`, keeping its name.
pub(super) fn moved_path(rel: &str, dest_dir: &str) -> String {
    child_path(dest_dir, basename(rel))
}

/// Where dropping `from` on a row of `dir` would put it, or `None` where there is no such move.
///
/// The three refusals are what a drag can ask for and a rename cannot: a folder onto itself, a
/// folder into something under it (which would move it inside its own new self), and a drop back
/// into the folder it is already in, which is a no-op and not worth a confirmation dialog. `dir`
/// is "" for the vault root.
pub fn move_dest(from: &str, dir: &str) -> Option<String> {
    let inside_itself = dir == from || dir.starts_with(&format!("{from}/"));
    if inside_itself || parent_dir(from) == dir {
        return None;
    }
    Some(moved_path(from, dir))
}

/// What Move to… moves into `dir`: each of `rels` that is not there already, keeping its name.
/// `Err` with the folder asked to go into itself or into what is under it, which refuses the
/// whole batch: moving the rest would be doing part of what was asked.
pub(super) fn moves_to(rels: &[String], dir: &str) -> Result<Vec<(String, String)>, String> {
    let mut moves = Vec::new();
    for rel in rels {
        if dir == rel || dir.starts_with(&format!("{rel}/")) {
            return Err(rel.clone());
        }
        if let Some(to) = move_dest(rel, dir) {
            moves.push((rel.clone(), to));
        }
    }
    Ok(moves)
}

/// The folder a Move to… entry names, from the vault root, "" being the root itself: every
/// segment is a folder, which [`split_typed`] reads as it reads the ones before a file's name.
pub(super) fn typed_dir(typed: &str) -> Result<String, &'static str> {
    split_typed("", &format!("{}/", typed.trim())).map(|(dir, _)| dir)
}

/// The paths no other one of them is inside, each with whether it is a folder, in their order.
/// A folder takes what is under it along, so a batch acts on it alone rather than on it and then
/// on files that have already gone with it.
pub fn topmost(paths: &[(String, bool)]) -> Vec<(String, bool)> {
    let all: std::collections::HashSet<&str> = paths.iter().map(|(rel, _)| rel.as_str()).collect();
    paths
        .iter()
        .filter(|(rel, _)| {
            !rel.match_indices('/')
                .any(|(at, _)| all.contains(&rel[..at]))
        })
        .cloned()
        .collect()
}

/// The folder a half-typed path points into and the last segment, which is the file's own name.
///
/// `base` is the folder the path is typed in: the file's own for Rename, the clicked row's for
/// New File. `..` walks back up out of it and stops at the vault root. A dot-named folder is taken
/// like any other, and one the vault never lists (`.git`, `.trash`) is refused. The name comes back
/// as typed, empty included, so that completion can read a path that is still being written.
pub(super) fn split_typed(base: &str, typed: &str) -> Result<(String, String), &'static str> {
    let typed = typed.trim();
    let (dirs, name) = typed.rsplit_once('/').unwrap_or(("", typed));
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    // Empty segments and `.` mean nothing here, so `a//b` and `./a` are the paths they look like.
    for dir in dirs
        .split('/')
        .map(str::trim)
        .filter(|d| !d.is_empty() && *d != ".")
    {
        match dir {
            ".." => {
                parts.pop().ok_or("That path leaves this vault.")?;
            }
            dir if out_of_reach(dir) => return Err(RESERVED),
            dir => parts.push(dir),
        }
    }
    Ok((parts.join("/"), name.trim().to_string()))
}

/// Where a typed name puts the file, vault-relative: a plain name lands in `dir`, and one carrying
/// `/` is a path relative to it, `..` walking back up out of it. `Err` where the path would leave
/// the vault or name something the vault never lists.
///
/// The extension is whatever was typed, in both dialogs. A note renamed out of `.md` stops being
/// one, which the dialog asks about rather than quietly preventing.
pub(super) fn typed_path(dir: &str, typed: &str) -> Result<String, &'static str> {
    let (dest, name) = split_typed(dir, typed)?;
    if name.is_empty() || name == "." || name == ".." {
        return Err("Enter a name.");
    }
    if out_of_reach(&name) {
        return Err(RESERVED);
    }
    Ok(child_path(&dest, &name))
}

/// Why a typed path may not name `.git`, `.trash` or a temporary: see [`out_of_reach`].
const RESERVED: &str = "That name is reserved.";

/// [`typed_path`] from the folder `rel` is in, which is what Rename types against.
pub(super) fn renamed_path(rel: &str, typed: &str) -> Result<String, &'static str> {
    typed_path(parent_dir(rel), typed)
}

/// What the toast calls it: a file that stayed in its folder was renamed, one that left it moved.
pub(super) fn verb(from: &str, to: &str) -> &'static str {
    match parent_dir(from) == parent_dir(to) {
        true => "Renamed",
        false => "Moved",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_lists_what_a_mkdir_p_would_have_made() {
        assert_eq!(levels("a/b/c").collect::<Vec<_>>(), ["a", "a/b", "a/b/c"]);
        assert_eq!(levels("a").collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn is_markdown_only_matches_a_markdown_extension() {
        assert!(is_markdown("note.md"));
        assert!(is_markdown("NOTE.MD"));
        assert!(is_markdown("note.markdown"));
        assert!(!is_markdown("note"));
        assert!(!is_markdown("chart.pdf"));
        assert!(!is_markdown("Notes"));
    }

    #[test]
    fn split_ext_keeps_the_last_dot_and_ignores_a_leading_one() {
        assert_eq!(split_ext("note.md"), ("note", ".md"));
        assert_eq!(split_ext("archive.tar.gz"), ("archive.tar", ".gz"));
        assert_eq!(split_ext("README"), ("README", ""));
        assert_eq!(split_ext(".gitignore"), (".gitignore", ""));
    }

    #[test]
    fn free_path_marks_a_copy_and_keeps_the_extension() {
        let free = |_: &str| false;
        assert_eq!(free_path("Notes", "a.md", false, free), "Notes/a.md");
        // "" is the vault root, and must not become a leading slash.
        assert_eq!(free_path("", "a.md", false, free), "a.md");

        // A paste beside its source always collides with it, which is what makes it a "(copy)".
        let held = |rel: &str| rel == "Notes/a.md";
        assert_eq!(free_path("Notes", "a.md", false, held), "Notes/a (copy).md");
        let two = |rel: &str| matches!(rel, "Notes/a.md" | "Notes/a (copy).md");
        assert_eq!(
            free_path("Notes", "a.md", false, two),
            "Notes/a (copy 2).md"
        );

        // A folder has no extension to keep, however many dots are in its name.
        let dotted = |rel: &str| rel == "Archive.2024";
        assert_eq!(
            free_path("", "Archive.2024", true, dotted),
            "Archive.2024 (copy)"
        );
        assert_eq!(
            free_path("", "Archive.2024", false, dotted),
            "Archive (copy).2024"
        );
    }

    #[test]
    fn renamed_part_leaves_a_file_its_extension_and_a_folder_nothing() {
        assert_eq!(renamed_part("note.md", false), "note");
        assert_eq!(renamed_part("Archive.2024", true), "Archive.2024");
    }

    #[test]
    fn topmost_drops_what_a_marked_folder_already_takes() {
        let paths = |list: &[&str]| -> Vec<(String, bool)> {
            list.iter().map(|rel| (rel.to_string(), false)).collect()
        };
        // A name that merely starts the same way is not inside the folder.
        assert_eq!(
            topmost(&paths(&["a", "a/b.md", "ab.md", "a/c/d.md"])),
            paths(&["a", "ab.md"])
        );
        assert_eq!(topmost(&paths(&["x/y.md"])), paths(&["x/y.md"]));
    }

    #[test]
    fn move_dest_refuses_the_three_moves_a_drop_can_ask_for() {
        // Into a folder, and out to the vault root.
        assert_eq!(move_dest("a/b.md", "x"), Some("x/b.md".into()));
        assert_eq!(move_dest("a/deep/b.md", ""), Some("b.md".into()));
        assert_eq!(move_dest("a/Notes", "x"), Some("x/Notes".into()));
        // Onto itself, and into what is under it.
        assert_eq!(move_dest("a/Notes", "a/Notes"), None);
        assert_eq!(move_dest("a/Notes", "a/Notes/Daily"), None);
        // Into the folder it is already in, which includes the root row over a root-level file.
        assert_eq!(move_dest("a/b.md", "a"), None);
        assert_eq!(move_dest("b.md", ""), None);
        // A folder whose name merely starts the same way is a different folder.
        assert_eq!(
            move_dest("a/Notes", "a/Notestore"),
            Some("a/Notestore/Notes".into())
        );
    }

    #[test]
    fn renamed_path_renames_in_place_and_moves_on_a_slash() {
        let to = |typed| renamed_path("Notes/Daily/mon.md", typed);
        assert_eq!(to("tue.md"), Ok("Notes/Daily/tue.md".into()));
        assert_eq!(
            to("Archive/tue.md"),
            Ok("Notes/Daily/Archive/tue.md".into())
        );
        // `..` walks up, which is the only way the keyboard reaches the vault root.
        assert_eq!(to("../tue.md"), Ok("Notes/tue.md".into()));
        assert_eq!(to("../../tue.md"), Ok("tue.md".into()));
        assert_eq!(to("../Archive/tue.md"), Ok("Notes/Archive/tue.md".into()));
        // One `..` too many leaves the vault, and so does one from a file already at the root.
        assert!(to("../../../tue.md").is_err());
        assert!(renamed_path("mon.md", "../tue.md").is_err());
        assert_eq!(renamed_path("a/x.pdf", "b/y.pdf"), Ok("a/b/y.pdf".into()));
        // Nothing typed, nothing but separators, and no name at all.
        assert!(to("").is_err());
        assert!(to("  ").is_err());
        assert!(to("..").is_err());
        assert!(to(".").is_err());
    }

    #[test]
    fn typed_path_takes_a_dot_named_name_but_nothing_the_vault_never_lists() {
        assert_eq!(typed_path("", ".gitignore"), Ok(".gitignore".into()));
        assert_eq!(
            typed_path("Code", ".config/init.lua"),
            Ok("Code/.config/init.lua".into())
        );
        // What is made under `.git` or `.trash` is never listed, whatever Show Hidden Files says.
        assert!(typed_path("", ".git/hooks/x").is_err());
        assert!(typed_path("Notes", ".trash").is_err());
    }

    #[test]
    fn renamed_path_lands_on_the_extension_that_was_typed() {
        let to = |typed| renamed_path("Notes/mon.md", typed);
        // Rename used to force `.md` back on, so a note could not become a source file.
        assert_eq!(to("main.rs"), Ok("Notes/main.rs".into()));
        // A bare name stays extensionless rather than becoming a note again.
        assert_eq!(to("notes"), Ok("Notes/notes".into()));
        assert_eq!(to("tue.md"), Ok("Notes/tue.md".into()));
        // A folder called `Notes` was never at risk, and still is not.
        assert_eq!(renamed_path("Notes", "Archive"), Ok("Archive".into()));
    }

    #[test]
    fn typed_path_names_a_folder_that_does_not_exist_yet() {
        // The dialog resolves the destination and `make_parents` creates what is missing; the
        // path arithmetic is the same whether the folder is there or not.
        assert_eq!(renamed_path("a.md", "New/x.md"), Ok("New/x.md".to_string()));
        assert_eq!(
            renamed_path("Notes/a.md", "New/Deep/x.md"),
            Ok("Notes/New/Deep/x.md".to_string())
        );
        // New File types against the clicked row's folder instead of the file's own.
        assert_eq!(
            typed_path("Notes", "Inbox/x.md"),
            Ok("Notes/Inbox/x.md".to_string())
        );
        assert_eq!(typed_path("", "x.md"), Ok("x.md".to_string()));
        assert!(typed_path("", "../x.md").is_err());
    }

    #[test]
    fn moved_path_keeps_the_basename() {
        assert_eq!(moved_path("a/b/c.md", "x/y"), "x/y/c.md");
        assert_eq!(moved_path("a/b/c.md", ""), "c.md");
        assert_eq!(moved_path("c.md", "x"), "x/c.md");
    }

    #[test]
    fn a_move_to_path_is_a_folder_from_the_vault_root() {
        assert_eq!(typed_dir(""), Ok(String::new()));
        // What the completion list puts in the entry ends in a slash.
        assert_eq!(typed_dir("Notes/Daily/"), Ok("Notes/Daily".into()));
        assert_eq!(typed_dir(" Notes//./Daily "), Ok("Notes/Daily".into()));
        assert_eq!(typed_dir("Notes/../Inbox"), Ok("Inbox".into()));
        assert!(typed_dir("../Elsewhere").is_err());
        assert!(typed_dir("Notes/.git").is_err());
    }

    #[test]
    fn move_to_leaves_out_what_is_there_already_and_refuses_a_folder_into_itself() {
        let rels = ["Notes/a.md".to_string(), "Inbox/b.md".to_string()];
        assert_eq!(
            moves_to(&rels, "Inbox"),
            Ok(vec![("Notes/a.md".to_string(), "Inbox/a.md".to_string())])
        );
        assert_eq!(moves_to(&rels[1..], "Inbox"), Ok(Vec::new()));
        let folder = ["Notes".to_string()];
        assert_eq!(moves_to(&folder, "Notes"), Err("Notes".to_string()));
        assert_eq!(moves_to(&folder, "Notes/Sub"), Err("Notes".to_string()));
        // A sibling whose name only starts the same is somewhere else.
        assert_eq!(
            moves_to(&folder, "Notes-old"),
            Ok(vec![("Notes".to_string(), "Notes-old/Notes".to_string())])
        );
    }
}
