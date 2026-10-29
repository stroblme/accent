//! A notebook on paper: the body of the page Print… and Export as HTML… write (DESIGN.md,
//! Notebook), each cell as its tab shows it. The `nb-*` and `ansi-*` classes are the window's
//! stylesheet's.

use super::output::{self, Attachment, Form, Output, Table};
use super::{Kind, Notebook};
use crate::markdown;

impl Notebook {
    /// The notebook at `rel`, a vault path, as an exported file holds it: a markdown cell as a
    /// note is ([`markdown::to_html_file`]), its `attachment:` pictures in place; a code cell's
    /// source beside its `[n]`, each output under it in the form [`output::pick`] gives the tab;
    /// a raw cell as preformatted text. A picture goes in as its file is, a `data:` URI.
    pub fn to_html_file(&self, rel: &str) -> String {
        let mut html = String::new();
        for cell in &self.cells {
            let (class, count) = match (cell.kind, cell.count()) {
                (Kind::Code, Some(n)) => ("nb-code", format!("[{n}]")),
                (Kind::Code, None) => ("nb-code", "[ ]".to_string()),
                (Kind::Markdown, _) => ("nb-markdown", String::new()),
                (Kind::Raw, _) => ("nb-raw", String::new()),
            };
            // A cell without a count keeps the column, so every cell's text starts in one place.
            html.push_str(&format!(
                "<div class=\"nb-cell {class}\"><div class=\"nb-count\">{count}</div>\
                 <div class=\"nb-body\">"
            ));
            match cell.kind {
                Kind::Markdown => html.push_str(&attachments_in(
                    &markdown::to_html_file(&cell.source, rel),
                    &cell.attachments,
                )),
                Kind::Code => {
                    html.push_str(&format!(
                        "<pre class=\"nb-source\"><code class=\"language-{}\">{}</code></pre>",
                        esc(&self.language),
                        esc(&cell.source)
                    ));
                    for out in cell.outputs() {
                        html.push_str(&format!(
                            "<div class=\"nb-output\">{}</div>",
                            output_html(out, rel)
                        ));
                    }
                }
                Kind::Raw => html.push_str(&format!(
                    "<pre class=\"nb-source\">{}</pre>",
                    esc(&cell.source)
                )),
            }
            html.push_str("</div></div>\n");
        }
        html
    }
}

/// One output in the form the tab shows it.
fn output_html(out: &Output, rel: &str) -> String {
    match out {
        Output::Stream { stderr, text } => pre(&output::carriage(text), *stderr),
        Output::Error {
            name,
            value,
            traceback,
        } => pre(&output::error_text(name, value, traceback), true),
        Output::Data { items, .. } => match output::pick(items) {
            Some(Form::Picture { mime, data }) => {
                format!("<img alt=\"\" src=\"{}\">", data_uri(mime, data))
            }
            Some(Form::Table(table)) => table_html(&table),
            Some(Form::Markdown(text)) => markdown::to_html_file(text, rel),
            Some(Form::Latex(latex)) => {
                let (tex, display) = output::tex(latex);
                match markdown::mathml(tex, display) {
                    Some(math) => format!("<div class=\"nb-math\">{math}</div>"),
                    None => pre(latex, false),
                }
            }
            Some(Form::Json(text)) => pre(&text, false),
            Some(Form::Text(text)) => pre(text, false),
            None => format!(
                "<p class=\"nb-placeholder\">{}</p>",
                esc(&output::placeholder(items))
            ),
        },
    }
}

/// Monospace text, its escape sequences turned into `ansi-N` and `ansi-bold` spans; tinted, as
/// the tab tints them, for stderr and a traceback.
fn pre(text: &str, stderr: bool) -> String {
    let (plain, spans) = output::ansi(text);
    let plain = plain.trim_end_matches('\n');
    let mut html = String::from(match stderr {
        true => "<pre class=\"nb-stderr\">",
        false => "<pre>",
    });
    let mut at = 0;
    for span in spans.iter().filter(|s| s.range.start < plain.len()) {
        let end = span.range.end.min(plain.len());
        html.push_str(&esc(&plain[at..span.range.start]));
        let mut classes: Vec<String> = span
            .colour
            .map(|n| format!("ansi-{n}"))
            .into_iter()
            .collect();
        if span.bold {
            classes.push("ansi-bold".into());
        }
        html.push_str(&format!(
            "<span class=\"{}\">{}</span>",
            classes.join(" "),
            esc(&plain[span.range.start..end])
        ));
        at = end;
    }
    html.push_str(&esc(&plain[at..]));
    html.push_str("</pre>");
    html
}

/// A data frame as a table of its own cells, never the notebook's HTML, then its notes.
fn table_html(table: &Table) -> String {
    let mut rows = vec![String::new(); table.rows];
    for cell in &table.cells {
        let tag = if cell.header { "th" } else { "td" };
        let mut span = String::new();
        if cell.rows > 1 {
            span.push_str(&format!(" rowspan=\"{}\"", cell.rows));
        }
        if cell.cols > 1 {
            span.push_str(&format!(" colspan=\"{}\"", cell.cols));
        }
        rows[cell.row].push_str(&format!("<{tag}{span}>{}</{tag}>", esc(&cell.text)));
    }
    let mut html = String::from("<table>");
    for row in rows {
        html.push_str(&format!("<tr>{row}</tr>"));
    }
    html.push_str("</table>");
    for note in &table.notes {
        html.push_str(&format!("<p class=\"nb-table-note\">{}</p>", esc(note)));
    }
    html
}

/// `html` with each `attachment:` picture a markdown cell shows as the `data:` URI of the one it
/// carries by that name; a picture it does not carry loses its `src`.
fn attachments_in(html: &str, attachments: &[Attachment]) -> String {
    const SRC: &str = " src=\"attachment:";
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(at) = rest.find(SRC) {
        let from = at + SRC.len();
        let Some(len) = rest[from..].find('"') else {
            break;
        };
        out.push_str(&rest[..at]);
        // The attribute is as the writer escapes it.
        let name = markdown::percent_decode(&rest[from..from + len].replace("&amp;", "&"));
        let picture =
            attachments.iter().filter(|a| a.name == name).find_map(|a| {
                match output::pick(&a.items) {
                    Some(Form::Picture { mime, data }) => Some(data_uri(mime, data)),
                    _ => None,
                }
            });
        if let Some(uri) = picture {
            out.push_str(&format!(" src=\"{uri}\""));
        }
        rest = &rest[from + len + 1..];
    }
    out.push_str(rest);
    out
}

/// A picture as a `data:` URI: a PNG or JPEG as the base64 the notebook stores, an SVG as its
/// text percent-encoded, so its own `<style>` cannot reach the page.
fn data_uri(mime: &str, data: &str) -> String {
    match mime {
        "image/svg+xml" => format!("data:{mime},{}", markdown::percent_encode(data)),
        _ => format!(
            "data:{mime};base64,{}",
            data.split_whitespace().collect::<String>()
        ),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use crate::notebook::read;

    const ANALYSIS: &str = include_str!("../../tests/fixtures/notebook/analysis.ipynb");

    #[test]
    fn a_notebook_on_paper_shows_what_its_tab_shows() {
        let nb = read("Notebooks/analysis.ipynb", ANALYSIS).unwrap();
        let html = nb.to_html_file("Notebooks/analysis.ipynb");
        let count = |s: &str| html.matches(s).count();
        // The plot and the markdown cell's attachment, as their files are; the SVG never inline.
        assert_eq!(count("<img alt=\"\" src=\"data:image/png;base64,iVBOR"), 1);
        assert_eq!(count("src=\"data:image/png;base64,"), 2, "{html}");
        assert_eq!(count("src=\"data:image/svg+xml,%3Csvg"), 1);
        assert_eq!(count("attachment:"), 0);
        assert_eq!(count("<svg"), 0);
        // The frame rebuilt from its cells, pandas' stylesheet left behind.
        assert_eq!(count("<table>"), 1);
        assert!(html.contains("<th colspan=\"2\">fidelity</th>"), "{html}");
        assert_eq!(count("<style"), 0);
        // The markdown cell's formula and sympy's.
        assert!(count("<math") >= 2, "{html}");
        // tqdm's bar played out, the traceback's colours as classes.
        assert!(html.contains("<pre class=\"nb-stderr\">100%|"), "{html}");
        assert!(html.contains("<span class=\"ansi-1\">ZeroDivisionError</span>"));
        assert_eq!(count("\x1b"), 0);
        assert_eq!(count("<script"), 0);
        assert!(html.contains("<div class=\"nb-count\">[3]</div>"), "{html}");
        assert!(html.contains("<div class=\"nb-count\">[ ]</div>"), "{html}");
        assert!(
            html.contains("<pre class=\"nb-source\">\\newpage</pre>"),
            "{html}"
        );
        assert!(html.contains("<code class=\"language-python\">import numpy as np\n"));
    }
}
