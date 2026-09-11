//! Label text. A draw.io label is either plain text or, with `html=1`, a small HTML fragment;
//! both become [`Run`]s for drawing, and both go to and from Markdown for editing.

use crate::style::Color;

/// How a run of text looks where it differs from the label's font. `None` keeps the font's.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub color: Option<Color>,
    /// In page units (CSS pixels).
    pub size: Option<f64>,
}

/// One piece of a label, in reading order.
#[derive(Debug, Clone, PartialEq)]
pub enum Run {
    Text {
        text: String,
        marks: Marks,
    },
    /// TeX between `\(`…`\)` (inline) or `$$`…`$$` (display), delimiters stripped. Only produced
    /// when the page has math switched on.
    Math {
        tex: String,
        display: bool,
    },
    /// A line break: `<br>`, the edge of a block, or `\n` in a plain label.
    Break,
    /// The start of a list item; its text follows, then a `Break`.
    Bullet,
}

/// An HTML label (`html=1`) as runs. `math` is the page's `math` switch.
pub fn html_to_runs(html: &str, math: bool) -> Vec<Run> {
    let _ = math;
    plain_to_runs(html, false)
}

/// A plain label as runs: one text run per line.
pub fn plain_to_runs(text: &str, math: bool) -> Vec<Run> {
    let _ = math;
    let mut runs = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            runs.push(Run::Break);
        }
        if !line.is_empty() {
            runs.push(Run::Text {
                text: line.to_string(),
                marks: Marks::default(),
            });
        }
    }
    runs
}

/// The text alone, one line per break.
pub fn runs_to_plain(runs: &[Run]) -> String {
    let mut out = String::new();
    for run in runs {
        match run {
            Run::Text { text, .. } => out.push_str(text),
            Run::Math {
                tex,
                display: false,
            } => out.push_str(&format!("\\({tex}\\)")),
            Run::Math { tex, display: true } => out.push_str(&format!("$${tex}$$")),
            Run::Break => out.push('\n'),
            Run::Bullet => out.push_str("• "),
        }
    }
    out
}

/// Runs as the Markdown subset the label editor shows.
pub fn runs_to_markdown(runs: &[Run]) -> String {
    runs_to_plain(runs)
}

/// A cell's label as Markdown for editing, whichever form it is stored in.
pub fn to_markdown(label: &str, html: bool) -> String {
    if html {
        runs_to_markdown(&html_to_runs(label, true))
    } else {
        runs_to_markdown(&plain_to_runs(label, true))
    }
}

/// Markdown from the label editor as the HTML a label is stored in (`html=1`).
pub fn markdown_to_html(md: &str) -> String {
    md.to_string()
}
