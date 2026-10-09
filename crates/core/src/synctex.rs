//! SyncTeX: which line of which source a point of a LaTeX-built PDF was typeset from, and where
//! on which page a source line went. The engines write it beside the PDF when run with
//! `-synctex=1` (`main.synctex.gz`, or `main.synctex` uncompressed); this reads the file itself,
//! so nothing of TeX's has to be installed for it (format: `synctex_parser_readme.txt` and
//! `synctex_parser.c` in TeX Live).
//!
//! The file is a line per record. Each page (`{3` to `}3`) nests boxes: `[` a vbox, `(` an hbox,
//! `h`/`v` an empty one; inside them `x`, `k`, `g` and `$` mark positions in a line of text. A
//! record names its input by tag (`Input:5:/path/./chapter.tex`) and line, then its position and
//! for a box its width, height and depth, in the engine's scaled points from the top-left of the
//! page. An hbox of a paragraph carries the line its paragraph ended on; the records inside it
//! carry the lines their words came from, which is why a hit is read off them.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

/// TeX's scaled points in a PDF point (a big point): 65536 × 72.27 / 72.
const SP_PER_BP: f32 = 65781.76;

/// Where a build into another directory beside the sources goes (`latexmk -outdir=build`).
const OUT_DIRS: [&str; 2] = ["build", "out"];

/// The rc file latexmk reads in the folder it runs in, the first of these it finds.
const RC_FILES: [&str; 2] = ["latexmkrc", ".latexmkrc"];

/// What a SyncTeX file is called beside its PDF, compressed first.
const EXTENSIONS: [&str; 2] = [".synctex.gz", ".synctex"];

/// What tells a PDF is a LaTeX build where it has no SyncTeX file: its source, or what a run
/// leaves beside it whatever its flags (`latexmk -pdf` without `-synctex=1`).
const RUN_FILES: [&str; 4] = [".tex", ".aux", ".fls", ".fdb_latexmk"];

/// The most SyncTeX files [`near`] offers: each may have to be read whole to know whether it
/// lists a source.
const NEAR: usize = 16;

/// The most a SyncTeX file may inflate to. A 126-page book of prose and formulas is 4 MB; only
/// a broken or hostile file comes near this.
const MAX_INFLATED: usize = 1 << 30;

/// A source line a point of the PDF was typeset from.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// The input as the engine named it, `./` and `..` taken out.
    pub file: PathBuf,
    /// 1-based, as TeX counts.
    pub line: u32,
}

/// Where a source line was typeset: a page, from 0, and the box of the line of text it went into,
/// in PDF points from the page's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub page: usize,
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// One SyncTeX file, read whole: its inputs and where each page's records are. A page's records
/// are parsed when a question about that page is asked.
pub struct Synctex {
    text: Vec<u8>,
    inputs: HashMap<u32, PathBuf>,
    /// Each page's number and the bytes between its `{n` and `}n` lines.
    pages: Vec<(u32, Range<usize>)>,
    /// PDF points per unit of the file, and the offset added to every position.
    unit: f32,
    offset: (f32, f32),
}

/// One record: its type character, the input tag and line it came from, its position, and for a
/// box its width, height and depth, all in the file's units.
#[derive(Debug, Clone, Copy)]
struct Record {
    kind: u8,
    tag: u32,
    line: u32,
    h: i64,
    v: i64,
    size: [i64; 3],
}

/// An hbox or an empty box of a page, with the records directly inside it.
struct Container {
    record: Record,
    children: Vec<Record>,
}

impl Synctex {
    /// Read `path`, gzip-compressed or not.
    pub fn read(path: &Path) -> crate::Result<Synctex> {
        let failed = |e| crate::Error::io(path.display(), e);
        let bytes = std::fs::read(path).map_err(failed)?;
        let text = match bytes.starts_with(&[0x1f, 0x8b]) {
            true => gunzip(&bytes).map_err(failed)?,
            false => bytes,
        };
        Ok(Synctex::parse(text, path.parent().unwrap_or(Path::new(""))))
    }

    /// The file's text; a relative input is taken to be in `dir`, the folder the file is in.
    pub fn parse(text: Vec<u8>, dir: &Path) -> Synctex {
        let (mut unit, mut mag, mut offset) = (8192.0, 1000.0, (578.0, 578.0));
        // The post scriptum's own, which `synctex update` writes over the preamble's.
        let (mut post_mag, mut post_offset): (Option<f32>, (Option<f32>, Option<f32>)) =
            (None, (None, None));
        let mut inputs = HashMap::new();
        let mut pages = Vec::new();
        let mut open: Option<(u32, usize)> = None;
        let mut post = false;
        let mut at = 0;
        for line in text.split(|b| *b == b'\n') {
            let start = at;
            at += line.len() + 1;
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            if let Some(rest) = line.strip_prefix("Input:") {
                if let Some((tag, name)) = rest.split_once(':')
                    && let Ok(tag) = tag.parse()
                {
                    inputs.insert(tag, normal(&dir.join(name)));
                }
            } else if let Some(n) = line.strip_prefix('{').and_then(|n| n.parse().ok()) {
                // LuaTeX opens a page twice.
                if open.is_none() {
                    open = Some((n, at));
                }
            } else if let Some(n) = line.strip_prefix('}').and_then(|n| n.parse::<u32>().ok()) {
                if let Some((number, from)) = open.take_if(|(number, _)| *number == n) {
                    pages.push((number, from..start));
                }
            } else if line == "Post scriptum:" {
                post = true;
            } else if let Some(value) = line.strip_prefix("Magnification:") {
                match post {
                    true => post_mag = value.trim().parse().ok(),
                    false => mag = value.trim().parse().unwrap_or(mag),
                }
            } else if let Some(value) = line.strip_prefix("Unit:") {
                unit = value.trim().parse().unwrap_or(unit);
            } else if let Some(value) = line.strip_prefix("X Offset:") {
                match post {
                    true => post_offset.0 = dimension(value),
                    false => offset.0 = value.trim().parse().unwrap_or(offset.0),
                }
            } else if let Some(value) = line.strip_prefix("Y Offset:") {
                match post {
                    true => post_offset.1 = dimension(value),
                    false => offset.1 = value.trim().parse().unwrap_or(offset.1),
                }
            }
        }
        // `synctex_parser.c`'s arithmetic: a unit is `Unit` scaled points, magnified by the
        // preamble's per mille and the post scriptum's factor; an offset is in the file's units
        // unless the post scriptum gives one in scaled points.
        let scale = unit / SP_PER_BP;
        let offset = (
            post_offset.0.map_or(offset.0 * scale, |sp| sp / SP_PER_BP),
            post_offset.1.map_or(offset.1 * scale, |sp| sp / SP_PER_BP),
        );
        Synctex {
            text,
            inputs,
            pages,
            unit: scale * post_mag.unwrap_or(1.0) * mag / 1000.0,
            offset,
        }
    }

    /// Whether `file` is one of the inputs, by its path with `./` and `..` taken out.
    pub fn has_input(&self, file: &Path) -> bool {
        let file = normal(file);
        self.inputs.values().any(|input| *input == file)
    }

    /// The source line typeset at `(x, y)` on `page` (from 0), in PDF points from its top-left
    /// corner. The innermost box under the point, or the nearest one; in it, the last record at
    /// or left of the point (the first of several at one place), or its first.
    pub fn edit(&self, page: usize, x: f32, y: f32) -> Option<Hit> {
        let containers = self.containers(page)?;
        let area = |c: &&Container| {
            let [l, t, r, b] = self.rect(&c.record);
            (r - l) * (b - t)
        };
        let solid: Vec<&Container> = containers.iter().filter(|c| area(c) > 0.0).collect();
        let inside = solid
            .iter()
            .filter(|c| {
                let [l, t, r, b] = self.rect(&c.record);
                (l..=r).contains(&x) && (t..=b).contains(&y)
            })
            .min_by(|a, b| area(a).total_cmp(&area(b)));
        let nearest = || {
            solid.iter().min_by(|a, b| {
                let away = |c: &Container| {
                    let [l, t, r, b] = self.rect(&c.record);
                    (l - x)
                        .max(x - r)
                        .max(0.0)
                        .hypot((t - y).max(y - b).max(0.0))
                };
                away(a).total_cmp(&away(b))
            })
        };
        let container = inside.or_else(nearest)?;
        let h = |r: &Record| self.point(r).0;
        let children = container.children.iter();
        let left = (children.clone().filter(|r| h(r) <= x))
            .reduce(|last, next| if h(next) > h(last) { next } else { last });
        let record = left
            .or_else(|| children.min_by(|a, b| h(a).total_cmp(&h(b))))
            .unwrap_or(&container.record);
        Some(Hit {
            file: self.inputs.get(&record.tag)?.clone(),
            line: record.line,
        })
    }

    /// Where line `line` (1-based) of `file` was typeset: the line of text its first record went
    /// into, which is where it starts, glue that [`fills`] a line out counting only where there is
    /// nothing else. A line nothing was typeset from (a command, a blank) goes to the nearest one
    /// below or above that was, below first.
    pub fn view(&self, file: &Path, line: u32) -> Option<Spot> {
        let file = normal(file);
        let tags: Vec<u32> = (self.inputs.iter())
            .filter(|(_, input)| **input == file)
            .map(|(tag, _)| *tag)
            .collect();
        // The page and the line of text each of the file's lines starts on, and whether that is
        // only where the glue filling a line out ended up: the one record of a paragraph's last
        // line, at times, and otherwise in the line of text before it.
        let mut starts: BTreeMap<u32, (usize, Record, bool)> = BTreeMap::new();
        self.walk(|page, record, line_box, fill| {
            if !tags.contains(&record.tag) {
                return;
            }
            let start = starts.entry(record.line).or_insert((page, *line_box, fill));
            if start.2 && !fill {
                *start = (page, *line_box, fill);
            }
        });
        let below = starts.range(line..).next();
        let above = starts.range(..line).next_back();
        let (_, (page, line_box, _)) = match (below, above) {
            (Some(b), Some(a)) if line - a.0 < b.0 - line => a,
            (Some(b), _) => b,
            (None, a) => a?,
        };
        let [left, top, right, bottom] = self.rect(line_box);
        Some(Spot {
            page: *page,
            left,
            top,
            right,
            bottom,
        })
    }

    /// The hboxes and empty boxes of `page` (from 0), each with what is directly inside it.
    fn containers(&self, page: usize) -> Option<Vec<Container>> {
        let number = u32::try_from(page + 1).ok()?;
        let (_, range) = self.pages.iter().find(|(n, _)| *n == number)?;
        let mut containers: Vec<Container> = Vec::new();
        // The boxes open around the record being read: an hbox's place in `containers`, or
        // `None` for a vbox, whose records are lines rather than positions along one.
        let mut stack: Vec<Option<usize>> = Vec::new();
        let mut last_v = 0;
        for line in self.text[range.clone()].split(|b| *b == b'\n') {
            match line.first() {
                Some(b')' | b']') => {
                    stack.pop();
                    continue;
                }
                None => continue,
                _ => {}
            }
            let Some(record) = record(line, &mut last_v) else {
                continue;
            };
            if let Some(Some(parent)) = stack.last()
                && !fills(&record, &containers[*parent].record)
            {
                containers[*parent].children.push(record);
            }
            match record.kind {
                b'(' | b'h' | b'v' => {
                    if record.kind == b'(' {
                        stack.push(Some(containers.len()));
                    }
                    containers.push(Container {
                        record,
                        children: Vec::new(),
                    });
                }
                b'[' => stack.push(None),
                _ => {}
            }
        }
        Some(containers)
    }

    /// Call `f` with every record of every page that marks a place in a line of text, in order:
    /// the page's number from 0, the record, the hbox that line is, and whether the record is
    /// the glue that [`fills`] it.
    fn walk(&self, mut f: impl FnMut(usize, &Record, &Record, bool)) {
        for (number, range) in &self.pages {
            let page = (*number as usize).saturating_sub(1);
            let mut stack: Vec<Option<Record>> = Vec::new();
            let mut last_v = 0;
            for line in self.text[range.clone()].split(|b| *b == b'\n') {
                match line.first() {
                    Some(b')' | b']') => {
                        stack.pop();
                        continue;
                    }
                    None => continue,
                    _ => {}
                }
                let Some(record) = record(line, &mut last_v) else {
                    continue;
                };
                match record.kind {
                    b'(' => stack.push(Some(record)),
                    b'[' => stack.push(None),
                    _ => {
                        // The line of text is the outermost hbox inside the innermost vbox (a
                        // superscript or a section number is a box in it), or an empty box
                        // itself where it is in no hbox; a kern or glue between the lines of a
                        // vbox is on no line of text.
                        let open = stack.iter().rev().map_while(Option::as_ref).last();
                        let empty = matches!(record.kind, b'h' | b'v');
                        let inner = stack.last().and_then(Option::as_ref);
                        if let Some(line_box) = open.or(empty.then_some(&record)) {
                            let fill = inner.is_some_and(|hbox| fills(&record, hbox));
                            f(page, &record, line_box, fill);
                        }
                    }
                }
            }
        }
    }

    /// A record's position in PDF points from the page's top-left corner.
    fn point(&self, record: &Record) -> (f32, f32) {
        (
            record.h as f32 * self.unit + self.offset.0,
            record.v as f32 * self.unit + self.offset.1,
        )
    }

    /// A box's left, top, right and bottom in PDF points: its position is the left end of its
    /// baseline, its height above that and its depth below. A record that is no box is a point.
    fn rect(&self, record: &Record) -> [f32; 4] {
        let (h, v) = self.point(record);
        let [w, height, depth] = record.size.map(|n| n as f32 * self.unit);
        [h.min(h + w), v - height, h.max(h + w), v + depth]
    }
}

/// Whether `record` is the glue or kern filling `hbox` out to its right end, which carries the
/// line that ended the paragraph (the blank line, the next `\section`) rather than one of its
/// words.
fn fills(record: &Record, hbox: &Record) -> bool {
    matches!(record.kind, b'k' | b'g') && record.h >= hbox.h + hbox.size[0]
}

/// One record line: `(5,12:8799518,8865054:22609920,647495,0`, `g5,12:10392041,8865054` and the
/// like. A `v` of `=` is the one the record before had. `None` for anything else.
fn record(line: &[u8], last_v: &mut i64) -> Option<Record> {
    let (&kind, rest) = line.split_first()?;
    // Not `x`, a position pdfTeX also writes at the start of each line of a paragraph with the
    // line the paragraph ended on.
    if !matches!(kind, b'(' | b'[' | b'h' | b'v' | b'k' | b'g' | b'$') {
        return None;
    }
    let mut parts = std::str::from_utf8(rest).ok()?.split(':');
    let mut link = parts.next()?.split(',');
    let (tag, line) = (link.next()?.parse().ok()?, link.next()?.parse().ok()?);
    let (h, v) = parts.next()?.split_once(',')?;
    let v = match v {
        "=" => *last_v,
        v => v.parse().ok()?,
    };
    *last_v = v;
    let mut size = [0; 3];
    if let Some(given) = parts.next() {
        for (slot, n) in size.iter_mut().zip(given.split(',')) {
            *slot = n.parse().ok()?;
        }
    }
    Some(Record {
        kind,
        tag,
        line,
        h: h.parse().ok()?,
        v,
        size,
    })
}

/// A post scriptum's offset, `12.5pt` or `-1in`, in scaled points; a bare number is in them.
fn dimension(value: &str) -> Option<f32> {
    let value = value.trim();
    let split = value
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number: f32 = number.trim().parse().ok()?;
    let per = match unit {
        "" | "sp" => 1.0,
        "pt" => 65536.0,
        "bp" => SP_PER_BP,
        "in" => 72.27 * 65536.0,
        "cm" => 72.27 * 65536.0 / 2.54,
        "mm" => 72.27 * 65536.0 / 25.4,
        "pc" => 12.0 * 65536.0,
        "dd" => 1238.0 / 1157.0 * 65536.0,
        "cc" => 12.0 * 1238.0 / 1157.0 * 65536.0,
        _ => return None,
    };
    Some(number * per)
}

/// `path` with `.` and `..` taken out, as the engines write `/home/me/thesis/./main.tex`.
fn normal(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// A gzip file's one member, inflated: its header (RFC 1952), then raw deflate.
fn gunzip(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let broken = || io::Error::new(io::ErrorKind::InvalidData, "not a gzip file");
    let flags = *bytes.get(3).ok_or_else(broken)?;
    let mut at = 10;
    if flags & 0x04 != 0 {
        let extra = bytes.get(at..at + 2).ok_or_else(broken)?;
        at += 2 + usize::from(u16::from_le_bytes([extra[0], extra[1]]));
    }
    // A file name, then a comment, each ending in a NUL.
    for flag in [0x08, 0x10] {
        if flags & flag != 0 {
            at += bytes
                .get(at..)
                .and_then(|rest| rest.iter().position(|b| *b == 0))
                .ok_or_else(broken)?
                + 1;
        }
    }
    if flags & 0x02 != 0 {
        at += 2;
    }
    miniz_oxide::inflate::decompress_to_vec_with_limit(
        bytes.get(at..).ok_or_else(broken)?,
        MAX_INFLATED,
    )
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))
}

/// The SyncTeX file a LaTeX build left beside `pdf` (`main.pdf`'s `main.synctex.gz`), if one is no
/// older than it: a PDF built since without SyncTeX, or written into since, would not match it.
pub fn beside(pdf: &Path) -> Option<PathBuf> {
    let stem = pdf.file_stem()?;
    let built = std::fs::metadata(pdf).and_then(|m| m.modified()).ok()?;
    EXTENSIONS.iter().find_map(|ext| {
        let mut name = stem.to_os_string();
        name.push(ext);
        let file = pdf.with_file_name(name);
        let written = std::fs::metadata(&file).and_then(|m| m.modified()).ok()?;
        (written >= built).then_some(file)
    })
}

/// The PDF a SyncTeX file is of: `main.synctex.gz`'s `main.pdf` beside it.
pub fn pdf_of(synctex: &Path) -> Option<PathBuf> {
    let name = synctex.file_name()?.to_str()?;
    let stem = EXTENSIONS.iter().find_map(|ext| name.strip_suffix(ext))?;
    Some(synctex.with_file_name(format!("{stem}.pdf")))
}

/// The SyncTeX files that may hold `tex`, below `root`, nearest first, each the one `build` takes
/// for the build of the PDF beside it ([`beside`], unless the caller knows more): the build of
/// `tex` itself, then every other one in the folders [`build_dirs`] names, for a file `\input` by
/// another. Whether one lists `tex` is for [`Synctex::has_input`] to say once it is read.
pub fn near(root: &Path, tex: &Path, build: impl Fn(&Path) -> Option<PathBuf>) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (tex.parent(), tex.file_stem()) else {
        return Vec::new();
    };
    let own = build_dirs(root, dir).flat_map(|(folder, below)| {
        let folder = folder.join(below);
        EXTENSIONS.map(|ext| {
            let mut name = stem.to_os_string();
            name.push(ext);
            folder.join(name)
        })
    });
    let others = build_dirs(root, dir).flat_map(|(folder, _)| {
        let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| pdf_of(path).is_some())
            .collect();
        files.sort();
        files
    });
    let mut found: Vec<PathBuf> = Vec::new();
    for file in own.chain(others) {
        let built = pdf_of(&file).and_then(|pdf| build(&pdf));
        if built.as_ref() == Some(&file) && !found.contains(&file) {
            found.push(file);
            if found.len() == NEAR {
                break;
            }
        }
    }
    found
}

/// Whether the PDF at `pdf` is a LaTeX build, SyncTeX file or not: a `.tex` of its name, or the
/// files a run leaves ([`RUN_FILES`]), beside it or in the folders [`build_dirs`] names.
pub fn is_build(root: &Path, pdf: &Path) -> bool {
    let (Some(dir), Some(stem)) = (pdf.parent(), pdf.file_stem()) else {
        return false;
    };
    build_dirs(root, dir).any(|(folder, below)| {
        RUN_FILES
            .iter()
            .any(|ext| folder.join(below).join(named(stem, ext)).is_file())
    })
}

/// The PDF a build of `tex` made, looked for where [`near`] looks for its SyncTeX file.
pub fn pdf_near(root: &Path, tex: &Path) -> Option<PathBuf> {
    let (dir, stem) = (tex.parent()?, tex.file_stem()?);
    build_dirs(root, dir)
        .map(|(folder, below)| folder.join(below).join(named(stem, ".pdf")))
        .find(|pdf| pdf.is_file())
}

/// `stem` with `ext` after it, which `with_extension` would put in place of a dot in the stem.
fn named(stem: &std::ffi::OsStr, ext: &str) -> std::ffi::OsString {
    let mut name = stem.to_os_string();
    name.push(ext);
    name
}

/// Where a LaTeX build of a file in `dir` may have written what it makes, nearest first: each
/// folder from `dir` up to `root`, and after each its `build/` and `out/` and the `$out_dir` and
/// `$aux_dir` a latexmk rc file there names (`rc_dirs`) inside `root`, each with the part of
/// `dir` below that folder, which a build into another directory mirrors. The outline's `.aux`
/// lookup reads the same folders.
pub fn build_dirs<'a>(root: &'a Path, dir: &'a Path) -> impl Iterator<Item = (PathBuf, &'a Path)> {
    dir.ancestors()
        .take_while(move |at| at.starts_with(root))
        .flat_map(move |at| {
            let below = dir.strip_prefix(at).unwrap_or(Path::new(""));
            let mut folders = vec![at.to_path_buf()];
            folders.extend(OUT_DIRS.map(|out| at.join(out)));
            let rc = RC_FILES
                .iter()
                .find_map(|name| std::fs::read_to_string(at.join(name)).ok());
            for named in rc.as_deref().map(rc_dirs).unwrap_or_default() {
                let folder = normal(&at.join(named));
                if folder.starts_with(root) && !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
            folders.into_iter().map(move |folder| (folder, below))
        })
}

/// The folders a latexmk rc file sends a build to: its `$out_dir`, then its `$aux_dir`, each
/// where the last assignment to it is a plain string (`$out_dir = 'build';`). Anything computed,
/// an interpolation, a concatenation, another variable, is not followed, and it forgets a plain
/// value before it.
fn rc_dirs(rc: &str) -> Vec<&str> {
    let (mut out, mut aux) = (None, None);
    for line in rc.lines() {
        for statement in line.split(';').map(str::trim) {
            if statement.starts_with('#') {
                break;
            }
            let Some((name, value)) = statement.split_once('=') else {
                continue;
            };
            let slot = match name.trim() {
                "$out_dir" => &mut out,
                "$aux_dir" => &mut aux,
                _ => continue,
            };
            *slot = literal(value.trim());
        }
    }
    out.into_iter().chain(aux).collect()
}

/// The text of a Perl string literal with nothing in it to interpolate or escape.
fn literal(value: &str) -> Option<&str> {
    let quote = value.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let text = value.strip_prefix(quote)?.strip_suffix(quote)?;
    (!text.is_empty() && !text.contains([quote, '\\', '$', '@'])).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pdflatex -synctex=1 main.tex` in `/tmp/synctex`, over the `main.tex` and `chapter.tex`
    /// beside it: two pages, the first holding a paragraph across three source lines, inline
    /// math, and `chapter.tex` by `\input`. The answers asserted are those `synctex edit` and
    /// `synctex view` (SyncTeX 1.21) give for the same points and lines, unless a comment says.
    fn fixture() -> Synctex {
        let bytes = include_bytes!("../tests/fixtures/synctex/main.synctex.gz");
        Synctex::parse(gunzip(bytes).unwrap(), Path::new("/tmp/synctex"))
    }

    fn hit(file: &str, line: u32) -> Option<Hit> {
        Some(Hit {
            file: PathBuf::from("/tmp/synctex").join(file),
            line,
        })
    }

    #[test]
    fn a_point_goes_to_the_source_line_its_words_came_from() {
        let s = fixture();
        // The section heading, and above it in the margin.
        assert_eq!(s.edit(0, 150.0, 140.0), hit("main.tex", 4));
        assert_eq!(s.edit(0, 150.0, 120.0), hit("main.tex", 4));
        // The paragraph's three lines of text, which each run across two source lines: its
        // hboxes all say line 7, where it ended.
        assert_eq!(s.edit(0, 300.0, 157.0), hit("main.tex", 5));
        assert_eq!(s.edit(0, 450.0, 157.0), hit("main.tex", 5));
        assert_eq!(s.edit(0, 150.0, 169.0), hit("main.tex", 6));
        assert_eq!(s.edit(0, 420.0, 169.0), hit("main.tex", 6));
        assert_eq!(s.edit(0, 200.0, 181.0), hit("main.tex", 7));
        // Inline math, on the baseline and in a superscript's own box.
        assert_eq!(s.edit(0, 300.0, 192.0), hit("main.tex", 9));
        assert_eq!(s.edit(0, 290.0, 189.0), hit("main.tex", 9));
        // The `\input` file, and below the last line of text.
        assert_eq!(s.edit(0, 160.0, 226.0), hit("chapter.tex", 1));
        assert_eq!(s.edit(0, 180.0, 249.0), hit("chapter.tex", 2));
        assert_eq!(s.edit(0, 400.0, 249.0), hit("chapter.tex", 3));
        assert_eq!(s.edit(0, 400.0, 400.0), hit("chapter.tex", 3));
        // The second page.
        assert_eq!(s.edit(1, 150.0, 120.0), hit("main.tex", 12));
        assert_eq!(s.edit(1, 150.0, 150.0), hit("main.tex", 13));
        assert_eq!(s.edit(2, 150.0, 150.0), None);
    }

    #[test]
    fn a_source_line_goes_to_the_line_of_text_it_starts_on() {
        let s = fixture();
        // One of `synctex view`'s answers, which gives the box as `h`, `v` (its bottom), `W` and
        // `H`: it lists every line of text a source line went into, and leads with another.
        let is = |file: &str, line, page, v: f32, height: f32| {
            let at = s.view(&Path::new("/tmp/synctex").join(file), line).unwrap();
            let want = [133.768_36, v - height, 133.768_36 + 343.711_06, v];
            let got = [at.left, at.top, at.right, at.bottom];
            let near = got.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.001);
            assert!(at.page == page && near, "{file}:{line} at {at:?}");
        };
        is("main.tex", 4, 0, 134.764_62, 9.843_078);
        is("main.tex", 5, 0, 158.522_72, 8.855_677);
        // The paragraph's lines of source each start at the end of a line of text.
        is("main.tex", 6, 0, 158.522_72, 8.855_677);
        is("main.tex", 7, 0, 170.477_89, 8.855_677);
        is("main.tex", 9, 0, 194.388_23, 10.046_797);
        is("main.tex", 13, 1, 158.522_72, 8.855_677);
        is("chapter.tex", 1, 0, 225.396_9, 9.843_078);
        is("chapter.tex", 2, 0, 249.155, 8.855_677);
        is("chapter.tex", 3, 0, 249.155, 8.855_677);
        // Nothing was typeset from the preamble or `\begin{document}`: the nearest line that
        // was, below first. The blank line ending a paragraph is its last line of text, where
        // the glue filling it out went (`synctex view` takes the line after).
        is("main.tex", 1, 0, 134.764_62, 9.843_078);
        is("main.tex", 3, 0, 134.764_62, 9.843_078);
        is("main.tex", 8, 0, 182.433_06, 8.855_677);
        // `./` as the engine writes it, and a file the build never read.
        assert!(s.has_input(Path::new("/tmp/synctex/./chapter.tex")));
        assert!(!s.has_input(Path::new("/tmp/synctex/other.tex")));
        assert_eq!(s.view(Path::new("/tmp/synctex/other.tex"), 1), None);
    }

    #[test]
    fn units_magnification_and_offsets_move_every_position() {
        let page = |preamble: &str, post: &str| {
            let text = format!(
                "SyncTeX Version:1\nInput:1:/a.tex\n{preamble}Content:\n{{1\n\
                 h1,1:65781760,65781760:6578176,657817,0\n}}1\nPostamble:\nPost scriptum:\n{post}"
            );
            let s = Synctex::parse(text.into_bytes(), Path::new("/"));
            s.view(Path::new("/a.tex"), 1)
                .map(|at| (at.left.round(), at.right.round()))
        };
        let plain = "Magnification:1000\nUnit:1\nX Offset:0\nY Offset:0\n";
        assert_eq!(page(plain, ""), Some((1000.0, 1100.0)));
        // Two scaled points to the unit, magnified twice over.
        let doubled = "Magnification:2000\nUnit:2\nX Offset:0\nY Offset:0\n";
        assert_eq!(page(doubled, ""), Some((4000.0, 4400.0)));
        // An offset in the preamble's units, and one in the post scriptum's dimensions.
        let moved = "Magnification:1000\nUnit:1\nX Offset:6578176\nY Offset:0\n";
        assert_eq!(page(moved, ""), Some((1100.0, 1200.0)));
        assert_eq!(page(plain, "X Offset:-1in\n"), Some((928.0, 1028.0)));
        assert_eq!(page(plain, "Magnification:0.5\n"), Some((500.0, 550.0)));
    }

    #[test]
    fn a_build_is_found_beside_its_pdf_and_from_the_sources_above_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |rel: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
            path
        };
        // `latexmk -outdir=build` from the thesis folder, and a figure built in place.
        write("thesis/main.tex");
        let chapter = write("thesis/chapters/one.tex");
        write("thesis/build/main.pdf");
        let main = write("thesis/build/main.synctex.gz");
        let fig_pdf = write("thesis/chapters/fig.pdf");
        let fig = write("thesis/chapters/fig.synctex");
        assert_eq!(
            beside(&root.join("thesis/build/main.pdf")),
            Some(main.clone())
        );
        assert_eq!(pdf_of(&fig), Some(fig_pdf.clone()));
        assert_eq!(near(root, &chapter, beside), [fig.clone(), main.clone()]);
        assert_eq!(
            near(root, &root.join("thesis/main.tex"), beside),
            vec![main.clone()]
        );
        assert_eq!(
            near(&root.join("thesis/chapters"), &chapter, beside),
            vec![fig]
        );
        // A PDF written since its SyncTeX file, rebuilt without one, has none.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write("thesis/chapters/fig.pdf");
        assert_eq!(beside(&fig_pdf), None);
        assert_eq!(near(root, &chapter, beside), [main]);
    }

    /// A PDF is a LaTeX build by its source or by what a run leaves beside it or in its build
    /// folder, SyncTeX file or not; a `.tex` finds its build's PDF where it would find the build.
    #[test]
    fn a_build_is_known_by_its_source_and_its_run_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |rel: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"").unwrap();
            path
        };
        // `latexmk -pdf main.tex` in place, a build into `build/` with only its `.fls` left,
        // and a PDF nothing built.
        let main = write("paper/main.tex");
        let main_pdf = write("paper/main.pdf");
        let report = write("report/report.tex");
        let report_pdf = write("report/build/report.pdf");
        write("report/build/report.fls");
        let scan = write("scans/scan.pdf");
        assert!(is_build(root, &main_pdf));
        assert!(is_build(root, &report_pdf));
        assert!(!is_build(root, &scan));
        assert_eq!(pdf_near(root, &main), Some(main_pdf));
        assert_eq!(pdf_near(root, &report), Some(report_pdf));
        assert_eq!(pdf_near(root, &write("paper/notes.tex")), None);
    }

    /// A latexmk rc file's `$out_dir` and `$aux_dir` are read where each is a plain string, the
    /// last assignment winning as in Perl; anything computed is left alone.
    #[test]
    fn a_latexmkrc_names_the_folders_a_build_goes_to() {
        assert_eq!(rc_dirs("$out_dir = 'build';\n"), ["build"]);
        assert_eq!(
            rc_dirs("$pdf_mode = 1;\n$aux_dir=\"tmp\" ; # the rest\n$out_dir = '_out/pdf';"),
            ["_out/pdf", "tmp"]
        );
        assert_eq!(rc_dirs("$out_dir = 'a'; $out_dir = 'b';"), ["b"]);
        // A computed value forgets the plain one before it: the build goes wherever that says.
        assert!(rc_dirs("$out_dir = 'a';\n$out_dir = $ENV{OUT};").is_empty());
        for rc in [
            "$out_dir = \"$ENV{HOME}/build\";",
            "$out_dir = 'a' . 'b';",
            "$out_dir = 'it\\'s';",
            "# $out_dir = 'build';",
            "$out_dir = '';",
            "$out_dir == 'build';",
        ] {
            assert!(rc_dirs(rc).is_empty(), "{rc}");
        }
    }

    /// A build sent elsewhere by the `.latexmkrc` above the sources is found as `build/`'s is,
    /// mirroring the folders below; one that would leave the vault is not looked in.
    #[test]
    fn a_build_where_a_latexmkrc_sends_it_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |rel: &str, text: &str| {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            path
        };
        write("thesis/.latexmkrc", "$out_dir = '_pdf';\n");
        write("thesis/main.tex", "");
        write("thesis/_pdf/main.pdf", "");
        let main = write("thesis/_pdf/main.synctex.gz", "");
        assert_eq!(near(root, &root.join("thesis/main.tex"), beside), [main]);
        let chapter = root.join("thesis/chapters");
        assert!(
            build_dirs(root, &chapter).any(|(folder, below)| folder == root.join("thesis/_pdf")
                && below == Path::new("chapters"))
        );
        write("latexmkrc", "$out_dir = '../elsewhere';\n");
        assert!(build_dirs(root, root).all(|(folder, _)| folder.starts_with(root)));
    }
}
