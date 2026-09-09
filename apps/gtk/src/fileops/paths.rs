//! Arithmetic on the paths the dialogs type and the tree drags: what a typed name resolves to,
//! where a drop may land, and what to call the move afterwards. Pure, and tested as such.

use accent_core::path::{basename, parent_dir};

/// Trim the typed name and refuse the ones that would not stay where they were put. A note
/// called `../x` escapes the vault, and a leading dot hides the file from the tree.
pub(super) fn sanitise_name(raw: &str) -> Result<String, &'static str> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("Enter a name.");
    }
    if name.contains('/') {
        return Err("Names cannot contain a slash.");
    }
    if name.starts_with('.') {
        return Err("Names cannot start with a dot.");
    }
    Ok(name.to_string())
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

/// `name` inside `dir`, where "" is the vault root.
pub(super) fn child_path(dir: &str, name: &str) -> String {
    match dir.trim_end_matches('/') {
        "" => name.to_string(),
        dir => format!("{dir}/{name}"),
    }
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

/// The folder a half-typed path points into and the last segment, which is the file's own name.
///
/// `base` is the folder the path is typed in: the file's own for Rename, the clicked row's for
/// New File. `..` walks back up out of it and stops at the vault root, and a segment starting with
/// a dot is refused because the tree hides one. The name comes back as typed, empty included, so
/// that completion can read a path that is still being written.
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
            dir if dir.starts_with('.') => return Err("Names cannot start with a dot."),
            dir => parts.push(dir),
        }
    }
    Ok((parts.join("/"), name.trim().to_string()))
}

/// Where a typed name puts the file, vault-relative: a plain name lands in `dir`, and one carrying
/// `/` is a path relative to it, `..` walking back up out of it. `Err` where the path would leave
/// the vault or name something the tree hides.
///
/// The extension is whatever was typed, in both dialogs. A note renamed out of `.md` stops being
/// one, which the dialog asks about rather than quietly preventing.
pub(super) fn typed_path(dir: &str, typed: &str) -> Result<String, &'static str> {
    let (dest, name) = split_typed(dir, typed)?;
    if name.is_empty() || name == ".." {
        return Err("Enter a name.");
    }
    if name.starts_with('.') {
        return Err("Names cannot start with a dot.");
    }
    Ok(child_path(&dest, &name))
}

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

/// Whether the file was already there, from an `anyhow` chain that has wrapped the `io::Error`.
pub(super) fn already_exists(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::AlreadyExists)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_name_trims_and_accepts_a_normal_name() {
        assert_eq!(
            sanitise_name("  Meeting notes  "),
            Ok("Meeting notes".into())
        );
        assert_eq!(
            sanitise_name("Übung 1 – Rückblick"),
            Ok("Übung 1 – Rückblick".into())
        );
        assert_eq!(sanitise_name("note.md"), Ok("note.md".into()));
    }

    #[test]
    fn sanitise_name_rejects_what_would_escape_the_vault() {
        assert!(sanitise_name("").is_err());
        assert!(sanitise_name("   ").is_err());
        assert!(sanitise_name("a/b").is_err());
        assert!(sanitise_name("../x").is_err());
        assert!(sanitise_name("..").is_err());
        assert!(sanitise_name(".hidden").is_err());
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
        // Nothing typed, nothing but separators, and a hidden name at either end.
        assert!(to("").is_err());
        assert!(to("  ").is_err());
        assert!(to("..").is_err());
        assert!(to(".hidden.md").is_err());
        assert!(to(".config/tue.md").is_err());
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
}
