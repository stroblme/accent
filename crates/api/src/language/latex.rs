//! texlab's outline of a LaTeX document, put right before it is shown.
//!
//! texlab numbers a heading by looking its title up in the table of contents the last build left
//! in the `.aux` file: two headings of one title both read the last one's number, a title holding
//! a command reads none (the toc writes `\emph{best}` as `\emph  {best}`), and a `\paragraph` or a
//! starred heading titled like a numbered one reads that one's number. Here the headings take the
//! toc's entries in order instead ([`Toc`]), from the file's own `.aux`, which is where a
//! document and every file it `\include`s leave theirs: beside it for a build in place, and for a
//! build into another directory (latexmk's `-outdir`) in a `build/` or `out/` next to it or next to
//! a folder above it, which mirrors the folders below. The numbers are the ones the PDF shows,
//! whatever the class makes of them (IEEEtran's `I-A`, memoir's `\chapternumberline`, a changed
//! `secnumdepth`), and like the PDF's they are the last build's.
//!
//! Counting the headings in the text instead would stay current between builds, but it cannot
//! number an `\include`d chapter, the chapters before it being in other files, nor anything a
//! class or a preamble numbers otherwise than article, report and book do.
//!
//! A file with no `.aux` of its own (an `\input` one) is numbered by the document that reads it,
//! whose `.aux` lists its headings among its own. texlab knows that document by its dependency
//! graph; here it is whichever `.aux` from the file's folder up to the vault root, or in a
//! `build/` or `out/` in one of them ([`Tocs::of`]), lists most of the file's headings by level and
//! title, in order ([`tidy`]). Titles are all it goes by, so a file `\input` twice, or two files
//! headed alike, read the first place the toc has them. With none listing any, the file keeps
//! texlab's numbers, less the one on a `\paragraph`, a `\subparagraph` or a starred heading, which
//! LaTeX never numbers.
//!
//! Every name is put on one line, texlab sending a title written over two lines as written.
//!
//! texlab also lists every display-math environment, labelled or not, and one inside another as
//! its child: an `aligned` in a labelled `equation` is a second row under the first. Only an
//! environment with a `\label` is listed; one without gives its place to what is inside it, which
//! keeps a label written in the inner environment listed.
//!
//! Inside `\input{…}` texlab offers only the `.tex` files; [`inputs`] lists the rest of the folder
//! being typed, an exported plot's `.pgf` say, which the document's words layer adds to its answer.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use accent_core::path;
use accent_core::synctex::build_dirs;
use accent_core::walk::FileKind;
use accent_lsp::types::DocumentSymbol;

use super::external::Encoding;
use super::{Completion, Kind, Range, byte_of};
use crate::{FileRow, locked};

/// The protocol's `Constant`, which is texlab's kind for a display-math environment.
const EQUATION: u32 = 14;

/// LaTeX's sectioning commands, which are the levels its table of contents names too.
const LEVELS: [&str; 7] = [
    "part",
    "chapter",
    "section",
    "subsection",
    "subsubsection",
    "paragraph",
    "subparagraph",
];

/// How many `.aux` files around a file with none of its own are read for the one numbering it.
const NEAR: usize = 16;

/// `symbols` as texlab answered them for `text`, put right: numbered by `own`, the toc of the
/// file's own build, or else by the one of `near` that lists most of its headings, the nearest of
/// those ([`Tocs::of`] finds both).
pub(super) fn tidy(
    symbols: Vec<DocumentSymbol>,
    text: &str,
    own: Option<&Toc>,
    near: &[Arc<Toc>],
    enc: Encoding,
) -> Vec<DocumentSymbol> {
    let mut toc = own.cloned().or_else(|| {
        let mut headings = Vec::new();
        numbered(&symbols, text, enc, &mut headings);
        let mut best: Option<(usize, usize, &Toc)> = None;
        for toc in near {
            if let Some((start, listed)) = toc.place(&headings)
                && listed > best.map_or(0, |(most, ..)| most)
            {
                best = Some((listed, start, toc));
            }
        }
        best.map(|(_, start, toc)| Toc {
            next: start,
            ..toc.clone()
        })
    });
    walk(symbols, text, enc, &mut toc)
}

/// The headings among `symbols` that LaTeX numbers, by level and [`key`], in the order [`walk`]
/// meets them.
fn numbered<'a>(
    symbols: &[DocumentSymbol],
    text: &'a str,
    enc: Encoding,
    out: &mut Vec<(&'a str, String)>,
) {
    for symbol in symbols {
        let start = byte_of(text, enc.char_pos(text, symbol.range.start));
        if let Some(h) = start.and_then(|at| Heading::parse(&text[at..]))
            && !h.starred
        {
            out.push((h.level, key(h.short.unwrap_or(h.title))));
        }
        numbered(
            symbol.children.as_deref().unwrap_or_default(),
            text,
            enc,
            out,
        );
    }
}

/// Every `.aux` table of contents read so far, by path, kept while the file has the modification
/// time it was read at: an outline is asked for on every edit, and the build rewrites the `.aux`
/// far less often.
#[derive(Default)]
pub(super) struct Tocs(Mutex<HashMap<PathBuf, (SystemTime, Arc<Toc>)>>);

impl Tocs {
    /// The tocs that may number `file`, below `root`: its own build's, and when it has none, the
    /// [`NEAR`] nearest others, from its folder up to `root`, each folder before its `build/` and
    /// `out/`. Only a toc that lists a heading counts.
    pub(super) fn of(&self, root: &Path, file: &Path) -> (Option<Arc<Toc>>, Vec<Arc<Toc>>) {
        let (Some(dir), Some(stem)) = (file.parent(), file.file_stem()) else {
            return (None, Vec::new());
        };
        let mut name = stem.to_os_string();
        name.push(".aux");
        // Beside the file, or where a build into `build/` or `out/` above it mirrors its folder.
        let own = build_dirs(root, dir)
            .find_map(|(folder, below)| self.read(&folder.join(below).join(&name)));
        if own.is_some() {
            return (own, Vec::new());
        }
        let auxes = build_dirs(root, dir).flat_map(|(folder, _)| {
            let mut auxes: Vec<PathBuf> = std::fs::read_dir(folder)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension() == Some(OsStr::new("aux")))
                .collect();
            auxes.sort();
            auxes
        });
        let near = auxes.take(NEAR).filter_map(|aux| self.read(&aux)).collect();
        (None, near)
    }

    /// The toc in the `.aux` at `path`, if there is one listing a heading.
    fn read(&self, path: &Path) -> Option<Arc<Toc>> {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
        let mut tocs = locked(&self.0);
        let toc = match tocs.get(path) {
            Some((at, toc)) if *at == modified => toc.clone(),
            _ => {
                let toc = Arc::new(Toc::parse(&std::fs::read_to_string(path).ok()?));
                tocs.insert(path.to_path_buf(), (modified, toc.clone()));
                toc
            }
        };
        // An `.aux` listing no heading is not the build of anything with headings.
        (!toc.entries.is_empty()).then_some(toc)
    }
}

/// [`tidy`], a heading before what it holds, so the headings meet the toc in document order.
fn walk(
    symbols: Vec<DocumentSymbol>,
    text: &str,
    enc: Encoding,
    toc: &mut Option<Toc>,
) -> Vec<DocumentSymbol> {
    symbols
        .into_iter()
        .flat_map(|mut symbol| {
            let children = symbol.children.take().unwrap_or_default();
            if symbol.kind == EQUATION && symbol.detail.is_none() {
                return walk(children, text, enc, toc);
            }
            let start = byte_of(text, enc.char_pos(text, symbol.range.start));
            symbol.name = match start.and_then(|at| Heading::parse(&text[at..])) {
                Some(heading) => heading.name(&symbol.name, toc.as_mut()),
                None => one_line(&symbol.name),
            };
            symbol.children = Some(walk(children, text, enc, toc));
            vec![symbol]
        })
        .collect()
}

/// A sectioning command as written: `\section*[short]{title}`.
struct Heading<'a> {
    level: &'a str,
    starred: bool,
    /// The title the toc takes instead, if there is one.
    short: Option<&'a str>,
    title: &'a str,
}

impl<'a> Heading<'a> {
    /// The heading `source` starts with, if it starts with one.
    fn parse(source: &'a str) -> Option<Self> {
        let (level, rest) = command(source)?;
        if !LEVELS.contains(&level) {
            return None;
        }
        let starred = rest.starts_with('*');
        let rest = rest.strip_prefix('*').unwrap_or(rest).trim_start();
        let (short, rest) = match rest.strip_prefix('[') {
            Some(inner) => inner
                .split_once(']')
                .map(|(short, rest)| (Some(short), rest))?,
            None => (None, rest),
        };
        let (title, _) = group(rest)?;
        Some(Self {
            level,
            starred,
            short,
            title,
        })
    }

    /// The heading's name in the outline, texlab having called it `texlab`: its number, if it has
    /// one, then its title.
    fn name(&self, texlab: &str, toc: Option<&mut Toc>) -> String {
        let title = one_line(self.title);
        let number = match toc {
            _ if self.starred => None,
            Some(toc) => toc.number(self.level, &key(self.short.unwrap_or(self.title))),
            None if matches!(self.level, "paragraph" | "subparagraph") => None,
            None => one_line(texlab)
                .strip_suffix(&title)
                .map(str::trim_end)
                .filter(|number| !number.is_empty())
                .map(String::from),
        };
        match number {
            Some(number) => format!("{number} {title}"),
            None => title,
        }
    }
}

/// The table of contents in an `.aux` file, handed to the headings in order.
#[derive(Clone)]
pub(super) struct Toc {
    entries: Vec<Entry>,
    /// The first entry no heading has taken or gone past.
    next: usize,
}

/// A line of the toc, `\contentsline {<level>}{\numberline {<number>}<title>}{<page>}…`, with no
/// `\numberline` for a heading LaTeX did not number.
#[derive(Clone)]
struct Entry {
    level: String,
    number: Option<String>,
    /// The title as [`key`] has it.
    key: String,
}

impl Toc {
    fn parse(aux: &str) -> Self {
        let entries = aux
            .split("\\contentsline")
            .skip(1)
            .filter_map(|line| {
                let (level, rest) = group(line)?;
                let (text, _) = group(rest)?;
                let (number, title) = match command(text) {
                    // memoir numbers a chapter by `\chapternumberline`.
                    Some((name, rest)) if name.ends_with("numberline") => {
                        group(rest).map(|(number, title)| (Some(number), title))?
                    }
                    _ => (None, text),
                };
                LEVELS.contains(&level).then(|| Entry {
                    level: level.to_string(),
                    number: number.map(|n| printed(n).filter(|c| !"{} ".contains(*c)).collect()),
                    key: key(title),
                })
            })
            .collect();
        Self { entries, next: 0 }
    }

    /// The number the last build gave the next heading, of `level` and titled `key`: that of the
    /// next entry of that level and title, the entries skipped over being those of headings
    /// deleted since the build or written by an `\input` file. A heading whose title the toc has
    /// otherwise (a macro in it expanded, the title edited since the build) takes the next entry
    /// instead when that one is of its level.
    fn number(&mut self, level: &str, key: &str) -> Option<String> {
        let (at, _) = self.find(self.next, level, key)?;
        self.next = at + 1;
        self.entries[at].number.clone()
    }

    /// The entry [`Toc::number`] hands a heading, looking from `from`, and whether it is the
    /// heading's by title as well as by level.
    fn find(&self, from: usize, level: &str, key: &str) -> Option<(usize, bool)> {
        let rest = &self.entries[from..];
        match rest
            .iter()
            .position(|entry| entry.level == level && entry.key == key)
        {
            Some(skip) => Some((from + skip, true)),
            None => (rest.first()?.level == level).then_some((from, false)),
        }
    }

    /// Where the headings of a file with no build of its own sit in this toc: the entry the first
    /// of them takes, and how many of them the toc lists by level and title, handed their entries
    /// as [`Toc::number`] hands them — how surely this is the build that reads the file. An
    /// `\input` file's headings are a run of entries, so of the entries of its first heading's
    /// level the one listing most of them is taken, then the one they spread over least. `None`
    /// where it lists none of them.
    fn place(&self, headings: &[(&str, String)]) -> Option<(usize, usize)> {
        let ((level, key), rest) = headings.split_first()?;
        let mut best: Option<(usize, usize, usize)> = None;
        let starts = self.entries.iter().enumerate();
        for (start, entry) in starts.filter(|(_, entry)| entry.level == *level) {
            let mut next = start + 1;
            let mut listed = usize::from(entry.key == *key);
            for (level, key) in rest {
                if let Some((at, titled)) = self.find(next, level, key) {
                    next = at + 1;
                    listed += usize::from(titled);
                }
            }
            let spread = next - start;
            let better = match best {
                Some((most, least, _)) => listed > most || listed == most && spread < least,
                None => true,
            };
            if better {
                best = Some((listed, spread, start));
            }
        }
        best.filter(|&(listed, ..)| listed > 0)
            .map(|(listed, _, start)| (start, listed))
    }
}

/// Where `\input{` is being typed when the caret, at `character` on `line`, is inside its
/// braces: the vault path of the folder named so far, read from the folder of the document at
/// `rel` as LaTeX reads it, and the column the file name being typed starts at. `None` outside
/// the braces, and for a folder outside the vault.
pub(super) fn input_dir(rel: &str, line: &str, character: u32) -> Option<(String, u32)> {
    const INPUT: &str = r"\input{";
    let head: String = line.chars().take(character as usize).collect();
    let open = head.rfind(INPUT)? + INPUT.len();
    let typed = &head[open..];
    if typed.contains('}') {
        return None;
    }
    let (folder, start) = match typed.rfind('/') {
        Some(at) => (&typed[..at], open + at + 1),
        None => ("", open),
    };
    let dir = path::parent_dir(rel);
    path::stays_inside(dir, folder).then(|| {
        let column = head[..start].chars().count() as u32;
        (path::resolve(dir, folder), column)
    })
}

/// What `\input{` offers beside texlab, which lists the `.tex` files and the folders: the
/// folder's other files, by their whole name, since LaTeX adds `.tex` only to a name without an
/// extension, and its folders, to go on into. `rows` is the folder's listing; a dot-named or a
/// sync-conflict row is left out, as the file tree leaves it out.
pub(super) fn inputs(rows: Vec<FileRow>, replace: Range) -> Vec<Completion> {
    rows.into_iter()
        .filter(|row| row.kind != FileKind::Conflict)
        .filter_map(|row| {
            let name = path::basename(&row.rel_path);
            let tex = row.kind != FileKind::Dir && name.ends_with(".tex");
            (!tex && !name.starts_with('.')).then(|| Completion {
                label: name.to_string(),
                kind: match row.kind {
                    FileKind::Dir => Kind::Folder,
                    _ => Kind::File,
                },
                detail: None,
                doc: None,
                filter: None,
                insert: name.to_string(),
                is_snippet: false,
                replace,
                extra_edits: Vec::new(),
                resolve: None,
            })
        })
        .collect()
}

/// The control word `text` starts with, without its backslash, and what follows it.
fn command(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('\\')?;
    let end = text
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    Some(text.split_at(end))
}

/// What is inside the `{…}` group `text` starts with, and what follows it.
fn group(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start().strip_prefix('{')?;
    let mut depth = 0;
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => _ = chars.next(),
            '{' => depth += 1,
            '}' if depth == 0 => return Some((&text[..at], &text[at + 1..])),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// A title as it is matched to the toc: its letters and digits, the commands in it left out,
/// the toc writing them otherwise (`\emph  {best}`) and the white space moved about.
fn key(title: &str) -> String {
    printed(&one_line(title))
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// `text` without its control words, the backslash of a control symbol (`\&`) included.
fn printed(text: &str) -> impl Iterator<Item = char> + '_ {
    let mut chars = text.chars().peekable();
    std::iter::from_fn(move || {
        loop {
            match chars.next()? {
                '\\' => while chars.next_if(char::is_ascii_alphabetic).is_some() {},
                c => return Some(c),
            }
        }
    })
}

/// `text` on one line as LaTeX reads it: without its comments, each run of white space one space.
fn one_line(text: &str) -> String {
    text.lines()
        .map(uncommented)
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

/// `line` up to its comment, if it has one.
fn uncommented(line: &str) -> &str {
    let comment = line
        .match_indices('%')
        .find(|&(at, _)| !line[..at].ends_with('\\'));
    comment.map_or(line, |(at, _)| &line[..at])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// A heading as texlab sends it, starting at the beginning of `line`.
    fn heading(name: &str, line: u32, children: Vec<Value>) -> Value {
        symbol(name, 2, None, line, children)
    }

    /// A math environment as texlab sends it, named by its label here rather than its number.
    fn equation(label: Option<&str>, line: u32, children: Vec<Value>) -> Value {
        let name = label.map_or("Equation".to_string(), |l| format!("Equation ({l})"));
        symbol(&name, EQUATION, label, line, children)
    }

    fn symbol(
        name: &str,
        kind: u32,
        label: Option<&str>,
        line: u32,
        children: Vec<Value>,
    ) -> Value {
        let at = json!({"start": {"line": line, "character": 0},
                        "end": {"line": line, "character": 0}});
        json!({"name": name, "kind": kind, "detail": label, "range": at,
               "selectionRange": at, "children": children})
    }

    /// The outline as `name [children]`, the way the pane indents it.
    fn names(symbols: &[DocumentSymbol]) -> Vec<String> {
        symbols
            .iter()
            .map(|s| {
                let inner = names(s.children.as_deref().unwrap_or_default());
                match inner.is_empty() {
                    true => s.name.clone(),
                    false => format!("{} [{}]", s.name, inner.join(", ")),
                }
            })
            .collect()
    }

    fn tidied(text: &str, aux: Option<&str>, answer: Vec<Value>) -> Vec<String> {
        tidied_near(text, aux, &[], answer)
    }

    /// [`tidied`] for a file whose own build left `aux`, with `near` the other `.aux` files around.
    fn tidied_near(
        text: &str,
        aux: Option<&str>,
        near: &[&str],
        answer: Vec<Value>,
    ) -> Vec<String> {
        let symbols = serde_json::from_value(Value::Array(answer)).unwrap();
        let own = aux.map(Toc::parse);
        let near: Vec<Arc<Toc>> = near.iter().map(|aux| Arc::new(Toc::parse(aux))).collect();
        names(&tidy(symbols, text, own.as_ref(), &near, Encoding::Utf16))
    }

    /// A toc line as pdflatex writes it.
    fn line(level: &str, number: &str, title: &str) -> String {
        format!(
            "\\@writefile{{toc}}{{\\contentsline {{{level}}}{{\\numberline {{{number}}}{title}}}{{1}}{{}}}}\n"
        )
    }

    /// `sections/intro.tex`, `\input` by `main.tex`, has no `.aux` of its own and texlab numbers
    /// its headings by title alone: its `Results` subsection as the section `Results` is. The
    /// build around it that lists its headings in order numbers them, not a nearer one listing
    /// fewer of them.
    #[test]
    fn a_file_with_no_aux_takes_its_numbers_from_the_build_listing_its_headings() {
        let text = "\\subsection{Results}\n\\subsection{Method}\n";
        let answer = vec![
            heading("3 Results", 0, vec![]),
            heading("Method", 1, vec![]),
        ];
        let other = line("subsection", "1.1", "Results");
        let main = [
            line("section", "1", "Intro"),
            line("subsection", "1.1", "Method"),
            line("section", "3", "Results"),
            line("subsection", "3.1", "Results"),
            line("subsection", "3.2", "Method"),
        ]
        .concat();
        assert_eq!(
            tidied_near(text, None, &[&other, &main], answer.clone()),
            ["3.1 Results", "3.2 Method"]
        );
        let unrelated = line("section", "1", "Elsewhere");
        assert_eq!(
            tidied_near(text, None, &[&unrelated], answer),
            ["3 Results", "Method"],
            "no build lists them: texlab's numbers"
        );
    }

    /// What pdflatex (latexmk `-outdir=build`) wrote for a `main.tex` with a `Results`
    /// subsection of its own before `\input{sections/results}`: the file's headings are the run
    /// of entries its `\input` left, not the first entry of their level and title.
    #[test]
    fn an_input_file_reads_the_run_of_entries_it_left() {
        let text = "\\subsection{Results}\n\\subsection{Discussion}\n\\subsection{Results}\n";
        let answer = vec![
            heading("3 Results", 0, vec![]),
            heading("3.2 Discussion", 1, vec![]),
            heading("3 Results", 2, vec![]),
        ];
        let main = r"\relax
\@writefile{toc}{\contentsline {section}{\numberline {1}Intro}{1}{}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {1.1}Results}{1}{}\protected@file@percent }
\@writefile{toc}{\contentsline {section}{\numberline {2}Methods}{1}{}\protected@file@percent }
\@writefile{toc}{\contentsline {section}{\numberline {3}Results}{1}{}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {3.1}Results}{1}{}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {3.2}Discussion}{1}{}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {3.3}Results}{1}{}\protected@file@percent }
\gdef \@abspage@last{1}
";
        assert_eq!(
            tidied_near(text, None, &[main], answer),
            ["3.1 Results", "3.2 Discussion", "3.3 Results"]
        );
    }

    /// The file's own build is found beside it, in a `build/` or `out/` there, or where an
    /// out-of-directory build of the document mirrors its folder; any other `.aux` from its
    /// folder up to the vault root is a candidate, nearest first, one listing nothing is not,
    /// and a rebuild is read again.
    #[test]
    fn the_builds_around_a_file_are_found_and_read_again_once_rebuilt() {
        let vault = tempfile::tempdir().unwrap();
        let root = vault.path();
        let write = |rel: &str, text: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("thesis/build/main.aux", &line("section", "1", "Intro"));
        write(
            "thesis/build/chapters/two.aux",
            &line("chapter", "2", "Two"),
        );
        write("thesis/sections/stray.aux", "\\relax\n");
        write("thesis/sections/out/x.aux", &line("section", "9", "Out"));
        write("other/far.aux", &line("section", "1", "Far"));
        let tocs = Tocs::default();
        let keys = |tocs: &[Arc<Toc>]| -> Vec<String> {
            tocs.iter().map(|t| t.entries[0].key.clone()).collect()
        };

        let (own, near) = tocs.of(root, &root.join("thesis/chapters/two.tex"));
        assert_eq!(keys(&own.into_iter().collect::<Vec<_>>()), ["Two"]);
        assert!(near.is_empty(), "its own build is enough");

        let intro = root.join("thesis/sections/intro.tex");
        let (own, near) = tocs.of(root, &intro);
        assert!(own.is_none());
        assert_eq!(keys(&near), ["Out", "Intro"]);

        let main = root.join("thesis/build/main.aux");
        std::fs::write(&main, line("section", "1", "Rebuilt")).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&main)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(keys(&tocs.of(root, &intro).1), ["Out", "Rebuilt"]);
    }

    /// An article as texlab 5.26 outlines it, with the `.aux` pdflatex wrote for it (hyperref
    /// loaded, `\foo` defined as "bar"): the toc's entries go to the headings in order.
    #[test]
    fn headings_take_their_numbers_from_the_toc_in_order() {
        let text = "\\section{Intro}\n\\subsection{Results}\n\\subsection{The \\emph{best} one}\n\
                    \\subsection{Results}\n\\section*{Starred}\n\\subsection{A long\ntitle}\n\
                    \\subsubsection{Deep $x^2$ \\foo}\n\\paragraph{Para}\n\\appendix\n\
                    \\section{Proofs}\n\\subsection{Lemma}\n";
        let aux = r"\relax
\providecommand\hyper@newdestlabel[2]{}
\@writefile{toc}{\contentsline {section}{\numberline {1}Intro}{1}{section.1}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {1.1}Results}{1}{subsection.1.1}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {1.2}The \emph  {best} one}{1}{subsection.1.2}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {1.3}Results}{1}{subsection.1.3}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {1.4}A long title}{1}{subsection.1.4}\protected@file@percent }
\@writefile{toc}{\contentsline {subsubsection}{\numberline {1.4.1}Deep $x^2$ bar}{1}{subsubsection.1.4.1}\protected@file@percent }
\@writefile{toc}{\contentsline {paragraph}{Para}{1}{section*.2}\protected@file@percent }
\@writefile{toc}{\contentsline {section}{\numberline {A}Proofs}{1}{appendix.A}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {A.1}Lemma}{1}{subsection.A.1}\protected@file@percent }
\gdef \@abspage@last{1}
";
        let deep = heading("Deep $x^2$ \\foo", 7, vec![heading("Para", 8, vec![])]);
        let answer = vec![
            heading(
                "1 Intro",
                0,
                vec![
                    heading("1.3 Results", 1, vec![]),
                    heading("The \\emph{best} one", 2, vec![]),
                    heading("1.3 Results", 3, vec![]),
                ],
            ),
            heading("Starred", 4, vec![heading("A long\ntitle", 5, vec![deep])]),
            heading("A Proofs", 10, vec![heading("A.1 Lemma", 11, vec![])]),
        ];
        assert_eq!(
            tidied(text, Some(aux), answer),
            [
                "1 Intro [1.1 Results, 1.2 The \\emph{best} one, 1.3 Results]",
                "Starred [1.4 A long title [1.4.1 Deep $x^2$ \\foo [Para]]]",
                "A Proofs [A.1 Lemma]"
            ]
        );
    }

    /// A report as texlab 5.26 outlines it, with its `.aux`: chapters number their sections, a
    /// `\subsubsection` is not numbered, a starred chapter is in the toc by `\addcontentsline`,
    /// and `\include{chapters/two}` left its entries in `chapters/two.aux`.
    #[test]
    fn a_report_numbers_sections_within_chapters() {
        let text = "\\chapter{One}\n\\section{Results}\n\\subsection{Sub}\n\\subsubsection{Deep}\n\
                    \\chapter*{Preface}\n\\addcontentsline{toc}{chapter}{Preface}\n\
                    \\include{chapters/two}\n\\part{Last}\n\\chapter{Three}\n\\section{Results}\n\
                    \\appendix\n\\chapter{App}\n\\section{Results}\n";
        let aux = r"\relax
\@writefile{toc}{\contentsline {chapter}{\numberline {1}One}{1}{}\protected@file@percent }
\@writefile{lof}{\addvspace {10\p@ }}
\@writefile{lot}{\addvspace {10\p@ }}
\@writefile{toc}{\contentsline {section}{\numberline {1.1}Results}{1}{}\protected@file@percent }
\@writefile{toc}{\contentsline {subsection}{\numberline {1.1.1}Sub}{1}{}\protected@file@percent }
\@writefile{toc}{\contentsline {subsubsection}{Deep}{1}{}\protected@file@percent }
\@writefile{toc}{\contentsline {chapter}{Preface}{2}{}\protected@file@percent }
\@input{chapters/two.aux}
\@writefile{toc}{\contentsline {part}{I\hspace  {1em}Last}{4}{}\protected@file@percent }
\@writefile{toc}{\contentsline {chapter}{\numberline {3}Three}{5}{}\protected@file@percent }
\@writefile{toc}{\contentsline {section}{\numberline {3.1}Results}{5}{}\protected@file@percent }
\@writefile{toc}{\contentsline {chapter}{\numberline {A}App}{6}{}\protected@file@percent }
\@writefile{toc}{\contentsline {section}{\numberline {A.1}Results}{6}{}\protected@file@percent }
\gdef \@abspage@last{6}
";
        let one = vec![heading(
            "A.1 Results",
            1,
            vec![heading(
                "1.1.1 Sub",
                2,
                vec![heading("1.1.1 Deep", 3, vec![])],
            )],
        )];
        let answer = vec![
            heading("1 One", 0, one),
            heading("Preface", 4, vec![]),
            heading(
                "Last",
                7,
                vec![
                    heading("3 Three", 8, vec![heading("A.1 Results", 9, vec![])]),
                    heading("A App", 11, vec![heading("A.1 Results", 12, vec![])]),
                ],
            ),
        ];
        assert_eq!(
            tidied(text, Some(aux), answer),
            [
                "1 One [1.1 Results [1.1.1 Sub [Deep]]]",
                "Preface",
                "Last [3 Three [3.1 Results], A App [A.1 Results]]"
            ]
        );
    }

    /// What texlab 5.26 answers once a build has numbered "Results" 1 and "Method" 1.1, for a
    /// file with no `.aux` beside it: texlab's numbers stay where LaTeX numbers the heading.
    #[test]
    fn paragraphs_and_starred_headings_lose_the_number_texlab_gave_them() {
        let text = "\\section{Results}\n\\subsection{Method}\n\\paragraph{ Results }\n\
                    \\subparagraph{Method}\n\\section*{Method}\n\\paragraph{2024 was a year}\n";
        let paragraph = heading("1 Results", 2, vec![heading("1.1 Method", 3, vec![])]);
        let answer = vec![
            heading(
                "1 Results",
                0,
                vec![heading("1.1 Method", 1, vec![paragraph])],
            ),
            heading("1.1 Method", 4, vec![heading("2024 was a year", 5, vec![])]),
        ];
        assert_eq!(
            tidied(text, None, answer),
            [
                "1 Results [1.1 Method [Results [Method]]]",
                "Method [2024 was a year]"
            ]
        );
    }

    /// Shaped as texlab 5.26 answers: an `aligned` in a labelled `equation`, a labelled `align`,
    /// a label written inside a `split`, and a bare `\[ … \]`.
    #[test]
    fn input_is_typed_from_the_documents_folder() {
        let at = |line: &str| input_dir("thesis/main.tex", line, line.chars().count() as u32);
        assert_eq!(at(r"\input{"), Some(("thesis".into(), 7)));
        assert_eq!(at(r"Ä \input{fig/pl"), Some(("thesis/fig".into(), 13)));
        assert_eq!(at(r"\input{../data/x"), Some(("data".into(), 15)));
        assert_eq!(at(r"\input{a} b"), None, "past the braces");
        assert_eq!(at(r"\include{"), None);
        assert_eq!(at(r"\input{../../x"), None, "outside the vault");
    }

    #[test]
    fn input_offers_the_files_texlab_leaves_out() {
        let row = |rel: &str, kind| FileRow {
            id: 1,
            rel_path: rel.into(),
            kind,
            title: None,
            size: 0,
            mtime_ns: 0,
            dependency: false,
        };
        let rows = vec![
            row("fig/plots", FileKind::Dir),
            row("fig/.cache", FileKind::Dir),
            row("fig/a.tex", FileKind::Other),
            row("fig/a.pgf", FileKind::Other),
            row(
                "fig/a.sync-conflict-20260101-000000-ABC.pgf",
                FileKind::Conflict,
            ),
            row("fig/table.csv", FileKind::Other),
        ];
        let items: Vec<(String, Kind)> = inputs(rows, Range::default())
            .into_iter()
            .map(|c| (c.insert, c.kind))
            .collect();
        assert_eq!(
            items,
            [
                ("plots".into(), Kind::Folder),
                ("a.pgf".into(), Kind::File),
                ("table.csv".into(), Kind::File),
            ]
        );
    }

    #[test]
    fn only_labelled_equations_are_listed() {
        let text = "\\section{Maths}\n\\begin{equation}\\label{eq:sum}\n\\begin{aligned}\n\
                    a &= b\n\\end{aligned}\n\\end{equation}\n\\begin{align}\n\
                    a &= b \\label{eq:row}\n\\end{align}\n\\begin{equation}\n\\begin{split}\n\
                    a &= b \\label{eq:inner}\n\\end{split}\n\\end{equation}\n\\[ c \\]\n";
        let answer = vec![heading(
            "Maths",
            0,
            vec![
                equation(Some("eq:sum"), 1, vec![equation(None, 2, vec![])]),
                equation(Some("eq:row"), 6, vec![]),
                equation(None, 9, vec![equation(Some("eq:inner"), 10, vec![])]),
                equation(None, 14, vec![]),
            ],
        )];
        assert_eq!(
            tidied(text, None, answer),
            ["Maths [Equation (eq:sum), Equation (eq:row), Equation (eq:inner)]"]
        );
    }
}
