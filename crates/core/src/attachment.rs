//! Where an image pasted or dropped into a note goes, and what the note calls it. Obsidian's rules,
//! so a vault kept in both apps files and embeds its images the same way.

use crate::path::{basename, parent_dir, resolve};

/// The folder a new attachment of the note `note` goes in, by the vault's `attachment_folder`
/// (Obsidian's "Default location for new attachments"): empty is the note's own folder, `./sub`
/// a folder inside it, and anything else a folder from the vault root, `/` being the root itself.
/// A `..` past the root is dropped, as it is from a link, so the answer is always in the vault.
pub fn folder(setting: &str, note: &str) -> String {
    let here = parent_dir(note);
    match setting.trim() {
        "" => here.to_string(),
        s if s == "." || s.starts_with("./") => resolve(here, s),
        s => resolve("", s),
    }
}

/// What a pasted image is called: `Pasted image 20260925143012.png`, the local time to the
/// second, as Obsidian names one.
pub fn pasted_name(now: chrono::NaiveDateTime) -> String {
    format!("Pasted image {}.png", now.format("%Y%m%d%H%M%S"))
}

/// Where `name` lands in `dir` without replacing anything: the name itself while it is free, then
/// `photo 1.png`, `photo 2.png`, … as Obsidian numbers them. Takes the lookup rather than the
/// vault, so every `stat` it costs stays on the caller's worker.
pub fn free_path(dir: &str, name: &str, taken: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    let at = |name: &str| match dir {
        "" => name.to_string(),
        dir => format!("{dir}/{name}"),
    };
    std::iter::once(at(name))
        .chain((1..).map(|n| at(&format!("{stem} {n}{ext}"))))
        .find(|rel| !taken(rel))
        .expect("a folder cannot hold every name")
}

/// The embed a note gets for the vault file `rel`: its name, `![[photo.png]]`, which the index
/// and Obsidian resolve by name — unless the name already finds another file (`by_name`, what
/// the index resolves it to), when it is the whole path, as Obsidian's "shortest path when
/// possible" writes it.
pub fn embed(rel: &str, by_name: Option<&str>) -> String {
    match by_name {
        Some(other) if other != rel => format!("![[{rel}]]"),
        _ => format!("![[{}]]", basename(rel)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_follows_the_setting_from_the_note() {
        let note = "Projects/Plan.md";
        assert_eq!(folder("", note), "Projects", "beside the note");
        assert_eq!(folder("", "Plan.md"), "", "beside a note at the root");
        assert_eq!(folder("./assets", note), "Projects/assets");
        assert_eq!(folder("./", note), "Projects");
        assert_eq!(folder("Attachments", note), "Attachments");
        assert_eq!(folder("/Media/img/", note), "Media/img");
        assert_eq!(folder("/", note), "", "the vault root");
        assert_eq!(folder("../../out", note), "out", "never out of the vault");
    }

    #[test]
    fn a_pasted_image_is_named_by_the_second() {
        let at = chrono::NaiveDate::from_ymd_opt(2026, 9, 5)
            .unwrap()
            .and_hms_opt(8, 4, 3)
            .unwrap();
        assert_eq!(pasted_name(at), "Pasted image 20260905080403.png");
    }

    #[test]
    fn a_taken_name_is_numbered_before_its_extension() {
        let taken = |rel: &str| ["a/photo.png", "a/photo 1.png", "x"].contains(&rel);
        assert_eq!(free_path("a", "photo.png", taken), "a/photo 2.png");
        assert_eq!(free_path("", "photo.png", taken), "photo.png");
        assert_eq!(free_path("", "x", taken), "x 1", "no extension");
    }

    #[test]
    fn an_embed_names_the_file_unless_the_name_finds_another() {
        assert_eq!(embed("a/p.png", None), "![[p.png]]", "not indexed yet");
        assert_eq!(embed("a/p.png", Some("a/p.png")), "![[p.png]]");
        assert_eq!(embed("a/p.png", Some("p.png")), "![[a/p.png]]");
    }
}
