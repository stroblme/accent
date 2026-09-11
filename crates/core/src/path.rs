//! Arithmetic on vault-relative paths: `"a/b/c.md"` and friends, always `/`-separated and never
//! absolute. One home for the split every layer used to redo with `rsplit_once('/')`.

use crate::markdown;

/// `"a/b/c.md"` -> `("a/b", "c.md")`; a name at the root gives `("", name)`.
fn split(rel: &str) -> (&str, &str) {
    match rel.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", rel),
    }
}

/// The file name: `"a/b/c.md"` -> `"c.md"`.
pub fn basename(rel: &str) -> &str {
    split(rel).1
}

/// The directory part: `"a/b/c.md"` -> `"a/b"`, `"c.md"` -> `""`.
pub fn parent_dir(rel: &str) -> &str {
    split(rel).0
}

/// The file name without its extension: `"a/b/c.md"` -> `"c"`.
pub fn stem(rel: &str) -> String {
    markdown::strip_ext(basename(rel))
}

/// What a file is, as far as its name says. It decides the icon a file list gives the row, and
/// nothing about how the file opens: whether a file is really text is a question for its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Note,
    Pdf,
    /// A draw.io diagram.
    Diagram,
    Table,
    Image,
    Code,
    Config,
    Text,
    Other,
}

/// Source files, by extension. Short on purpose: an unknown one is `Other`, which costs nothing
/// but a generic icon.
const CODE_EXT: &[&str] = &[
    "rs", "py", "c", "h", "cc", "cpp", "hpp", "js", "mjs", "jsx", "ts", "tsx", "go", "java", "kt",
    "swift", "rb", "php", "lua", "sh", "bash", "zsh", "fish", "r", "jl", "hs", "ml", "scala", "cs",
    "zig", "sql", "html", "css", "scss", "vue", "svelte", "tex", "vim", "el", "nix", "mk", "cmake",
];
/// Source files named for the tool that reads them rather than by an extension.
const CODE_NAMES: &[&str] = &[
    "makefile",
    "gnumakefile",
    "dockerfile",
    "containerfile",
    "justfile",
    "cmakelists.txt",
];
const CONFIG_EXT: &[&str] = &[
    "json", "jsonc", "toml", "yaml", "yml", "ini", "cfg", "conf", "env", "xml", "lock",
];
const TEXT_EXT: &[&str] = &["txt", "log", "rst", "org", "adoc"];

/// What `rel` is by its name alone. The extension is the file name's, lowercased, with a leading
/// dot counted as part of the name, as [`markdown::strip_ext`] has it: `LICENSE` and `.gitignore`
/// have none, and a file with no extension is text.
pub fn file_type(rel: &str) -> FileType {
    let name = basename(rel).to_ascii_lowercase();
    if CODE_NAMES.contains(&name.as_str()) {
        return FileType::Code;
    }
    if is_diagram(&name) {
        return FileType::Diagram;
    }
    let Some((_, ext)) = name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) else {
        return FileType::Text;
    };
    match ext {
        "md" | "markdown" => FileType::Note,
        "pdf" => FileType::Pdf,
        "csv" | "tsv" => FileType::Table,
        _ if markdown::is_image(&name) => FileType::Image,
        _ if CODE_EXT.contains(&ext) => FileType::Code,
        _ if CONFIG_EXT.contains(&ext) => FileType::Config,
        _ if TEXT_EXT.contains(&ext) => FileType::Text,
        _ => FileType::Other,
    }
}

/// Whether `rel` is a draw.io diagram by its name: `.drawio`, `.dio`, or `.drawio.xml`, the three
/// names draw.io saves under. A plain `.xml` holding a diagram is only known by its bytes.
pub fn is_diagram(rel: &str) -> bool {
    let name = basename(rel).to_ascii_lowercase();
    [".drawio", ".dio", ".drawio.xml"]
        .iter()
        .any(|ext| name.len() > ext.len() && name.ends_with(ext))
}

/// Where a link written inside the note at `dir` points, as a vault-relative path: `../a.md`
/// from `sub/deep` is `sub/a.md`, `./a.md` from `sub` is `sub/a.md`. A `..` past the root is
/// dropped rather than kept, and a leading `/` means the root, the way a site-absolute link does.
pub fn resolve(dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = match target.strip_prefix('/') {
        Some(_) => Vec::new(),
        None => dir.split('/').filter(|s| !s.is_empty()).collect(),
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Whether a link written inside the note at `dir` stays in the vault without [`resolve`]'s help:
/// relative, and never more `..` than there are folders to climb out of.
pub fn stays_inside(dir: &str, target: &str) -> bool {
    if target.starts_with('/') {
        return false;
    }
    let mut depth = dir.split('/').filter(|s| !s.is_empty()).count();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." if depth == 0 => return false,
            ".." => depth -= 1,
            _ => depth += 1,
        }
    }
    true
}

/// The link from a note at `dir` to the vault path `rel`, which [`resolve`] turns back into
/// `rel`: `sub/a.md` from `sub/deep` is `../a.md`, and from the root it is `sub/a.md`.
pub fn relative(dir: &str, rel: &str) -> String {
    let from: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    let to: Vec<&str> = rel.split('/').collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts = vec![".."; from.len() - shared];
    parts.extend(&to[shared..]);
    parts.join("/")
}

/// The half-open `rel_path` range that is exactly the descendants of `rel`: `[rel/, rel0)`,
/// because `'0'` is the byte after `'/'`. Keeps a subtree query on the `rel_path` index where a
/// `LIKE` would fall back to a scan.
pub fn subtree_range(rel: &str) -> (String, String) {
    (format!("{rel}/"), format!("{rel}0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_vault_relative_path() {
        assert_eq!((parent_dir("a.md"), basename("a.md")), ("", "a.md"));
        assert_eq!(
            (parent_dir("a/b/c.md"), basename("a/b/c.md")),
            ("a/b", "c.md")
        );
        assert_eq!(stem("sub/Rev 1.2.md"), "Rev 1.2");
        assert_eq!(stem("Makefile"), "Makefile");
        assert_eq!(subtree_range("a/b"), ("a/b/".into(), "a/b0".into()));
    }

    #[test]
    fn a_file_type_is_read_off_the_name() {
        for (rel, want) in [
            ("Notes/A.MD", FileType::Note),
            ("a.markdown", FileType::Note),
            ("Attachments/paper.pdf", FileType::Pdf),
            ("Figures/flow.drawio", FileType::Diagram),
            ("a.DIO", FileType::Diagram),
            ("export.drawio.xml", FileType::Diagram),
            ("plain.xml", FileType::Config),
            (".drawio", FileType::Text),
            ("data.csv", FileType::Table),
            ("data.tsv", FileType::Table),
            ("shot.PNG", FileType::Image),
            ("logo.svg", FileType::Image),
            ("src/main.rs", FileType::Code),
            ("tool.py", FileType::Code),
            ("Makefile", FileType::Code),
            ("sub/justfile", FileType::Code),
            ("package.json", FileType::Config),
            ("Cargo.toml", FileType::Config),
            ("notes.txt", FileType::Text),
            ("LICENSE", FileType::Text),
            // The dot belongs to the directory, so the file has no extension.
            ("v1.2/README", FileType::Text),
            // A leading dot is part of the name, as `strip_ext` has it.
            (".gitignore", FileType::Text),
            ("archive.zip", FileType::Other),
            ("mystery.xyz", FileType::Other),
        ] {
            assert_eq!(file_type(rel), want, "{rel}");
        }
    }

    #[test]
    fn resolves_a_link_against_the_note_that_holds_it() {
        assert_eq!(resolve("sub/deep", "../a.md"), "sub/a.md");
        assert_eq!(resolve("sub", "./a.md"), "sub/a.md");
        assert_eq!(resolve("sub", "a.md"), "sub/a.md");
        assert_eq!(resolve("", "a.md"), "a.md");
        assert_eq!(resolve("sub", "../../a.md"), "a.md");
        assert_eq!(resolve("sub", "/a.md"), "a.md");

        // `resolve` maps the last two into the vault too, but they did not point there.
        assert!(stays_inside("sub/deep", "../a.md"));
        assert!(stays_inside("sub", "./x/../../a.md"));
        assert!(!stays_inside("sub", "../../a.md"));
        assert!(!stays_inside("sub", "/a.md"));
    }

    #[test]
    fn a_relative_link_resolves_back_to_its_path() {
        assert_eq!(relative("sub/deep", "sub/a.md"), "../a.md");
        assert_eq!(relative("sub", "sub/a.md"), "a.md");
        assert_eq!(relative("", "Attachments/x.png"), "Attachments/x.png");
        // A shared prefix of the names is not a shared folder.
        assert_eq!(relative("sub", "sub2/a.md"), "../sub2/a.md");
        for (dir, rel) in [
            ("a/b/c", "a/x/y.md"),
            ("a", "b.md"),
            ("", "b.md"),
            ("x/y", "x/y.md"),
            ("a/b", "a/b/c/d.pdf"),
        ] {
            assert_eq!(resolve(dir, &relative(dir, rel)), rel, "{rel} from {dir:?}");
        }
    }
}
