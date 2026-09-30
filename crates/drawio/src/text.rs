//! A diagram's labels as text: one line per label, which is what search indexes and finds in a
//! diagram, and the labels rewritten through the model, which is what Replace All does to one.
//! The XML itself is never searched or rewritten, so a style key or an id is never a hit.

use crate::label::{self, Run};
use crate::model::{Cell, CellId, File};

/// One label as search reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    /// Its page's place in the file, from 0.
    pub page: usize,
    pub cell: CellId,
    /// What a reader sees, on one line: markup left out, a formula as written, whitespace
    /// collapsed.
    pub text: String,
}

/// Every shape's and connector's label that shows any text, page by page and each page's in
/// document order.
pub fn labels(file: &File) -> Vec<Label> {
    let mut out = Vec::new();
    for (page, p) in file.pages.iter().enumerate() {
        for cell in p.cells.iter().filter(|c| c.vertex || c.edge) {
            let text = line(cell);
            if !text.is_empty() {
                out.push(Label {
                    page,
                    cell: cell.id.clone(),
                    text,
                });
            }
        }
    }
    out
}

/// The labels as the index holds them: one line each, in [`labels`]' order.
pub fn search_text(file: &File) -> String {
    let lines: Vec<String> = labels(file).into_iter().map(|l| l.text).collect();
    lines.join("\n")
}

/// The label whose line in [`search_text`] holds byte `at`: its page and cell.
pub fn label_at(file: &File, at: usize) -> Option<(usize, CellId)> {
    let mut start = 0;
    for label in labels(file) {
        let end = start + label.text.len();
        if at <= end {
            return Some((label.page, label.cell));
        }
        start = end + 1;
    }
    None
}

/// Rewrite the text of every label through `edit`, which answers `None` for text it leaves
/// alone. A plain label is handed over whole; an HTML one text run by text run, its markup kept
/// as it was, so a match cannot reach across a tag. Returns how many labels changed.
pub fn edit_labels(file: &mut File, mut edit: impl FnMut(&str) -> Option<String>) -> usize {
    let mut changed = 0;
    for page in &mut file.pages {
        for cell in page.cells.iter_mut().filter(|c| c.vertex || c.edge) {
            let old = cell.label();
            let new = match cell.is_html() {
                true => edit_html(old, &mut edit),
                false => edit(old),
            };
            if let Some(new) = new.filter(|new| new != old) {
                cell.set_label(&new);
                changed += 1;
            }
        }
    }
    changed
}

/// A label on one line, as [`Label::text`] has it.
fn line(cell: &Cell) -> String {
    let text = match cell.is_html() {
        true => label::html_to_runs(cell.label(), false)
            .iter()
            .map(|run| match run {
                Run::Text { text, .. } => text.as_str(),
                Run::Math { tex, .. } => tex.as_str(),
                Run::Break | Run::Bullet => " ",
            })
            .collect(),
        false => cell.label().to_string(),
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `html` with each run of text between its tags passed through `edit`, decoded, and written
/// back escaped; `None` when `edit` changed nothing.
fn edit_html(html: &str, edit: &mut impl FnMut(&str) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(html.len());
    let mut changed = false;
    for (text, piece) in label::html_pieces(html) {
        match text.then(|| edit(&label::decode(piece))).flatten() {
            Some(new) => {
                changed = true;
                out.push_str(&escape(&new));
            }
            None => out.push_str(piece),
        }
    }
    changed.then_some(out)
}

/// Text as HTML writes it.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Point, Rect};
    use crate::model::Page;

    fn file() -> File {
        let mut one = Page::blank("One", "p1");
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        one.cells.extend([
            Cell::new_vertex("a", "1", r, "fillColor=#abcdef;", "Plain\n  text"),
            Cell::new_vertex("b", "1", r, "html=1;", "<b>Bold</b> &amp;<br>more"),
            Cell::new_vertex("empty", "1", r, "html=1;", "<br>"),
        ]);
        let mut two = Page::blank("Two", "p2");
        let mut edge = Cell::new_edge(
            "e",
            "1",
            (None, Point::default()),
            (None, Point::new(10.0, 0.0)),
            "",
        );
        edge.set_label(r"area \(x^2\)");
        two.cells.push(edge);
        let mut file = File::blank();
        file.pages = vec![one, two];
        file
    }

    #[test]
    fn a_diagram_reads_as_one_line_per_label() {
        let f = file();
        assert_eq!(
            search_text(&f),
            "Plain text\nBold & more\narea \\(x^2\\)",
            "no markup, no style keys, and a formula as written"
        );
        let pages: Vec<usize> = labels(&f).iter().map(|l| l.page).collect();
        assert_eq!(pages, [0, 0, 1]);
        assert_eq!(label_at(&f, 0), Some((0, "a".to_string())));
        assert_eq!(label_at(&f, 11), Some((0, "b".to_string())));
        assert_eq!(label_at(&f, 23), Some((1, "e".to_string())));
        assert_eq!(label_at(&f, 999), None);
    }

    #[test]
    fn labels_are_rewritten_as_text_and_their_markup_kept() {
        let mut f = file();
        let n = edit_labels(&mut f, |t| t.contains('&').then(|| t.replace('&', "<and>")));
        assert_eq!(n, 1);
        assert_eq!(
            f.pages[0].cells[3].label(),
            "<b>Bold</b> &lt;and&gt;<br>more"
        );
        let n = edit_labels(&mut f, |t| {
            t.contains("Plain").then(|| t.replace("Plain", "Clear"))
        });
        assert_eq!(n, 1);
        assert_eq!(f.pages[0].cells[2].label(), "Clear\n  text");
        assert_eq!(f.pages[0].cells[2].style.get("fillColor"), Some("#abcdef"));
        assert_eq!(edit_labels(&mut f, |_| None), 0);
    }
}
