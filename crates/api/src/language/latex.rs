//! texlab's outline of a LaTeX document, put right before it is shown.
//!
//! texlab numbers a heading by finding its title in the table of contents the last build left in
//! the `.aux` file, so a `\paragraph` or a starred heading titled like a numbered one is given
//! that one's number: `\paragraph{Results}` under `\subsection{Results}` reads "1.1 Results".
//! LaTeX numbers neither, so the number is taken off again.
//!
//! texlab also lists every display-math environment, labelled or not, and one inside another as
//! its child: an `aligned` in a labelled `equation` is a second row under the first. Only an
//! environment with a `\label` is listed; one without gives its place to what is inside it, which
//! keeps a label written in the inner environment listed.

use accent_lsp::types::DocumentSymbol;

use super::byte_of;
use super::external::Encoding;

/// The protocol's `Constant`, which is texlab's kind for a display-math environment.
const EQUATION: u32 = 14;

/// `symbols` as texlab answered them for `text`, put right.
pub(super) fn tidy(symbols: Vec<DocumentSymbol>, text: &str, enc: Encoding) -> Vec<DocumentSymbol> {
    symbols
        .into_iter()
        .flat_map(|mut symbol| {
            let children = tidy(symbol.children.take().unwrap_or_default(), text, enc);
            if symbol.kind == EQUATION && symbol.detail.is_none() {
                return children;
            }
            let start = byte_of(text, enc.char_pos(text, symbol.range.start));
            if let Some(title) = start.and_then(|at| unnumbered(&symbol.name, &text[at..])) {
                symbol.name = title.to_string();
            }
            symbol.children = Some(children);
            vec![symbol]
        })
        .collect()
}

/// `name` without the number texlab put in front of it, when the heading `source` starts with is
/// one LaTeX leaves unnumbered: a `\paragraph`, a `\subparagraph` or a starred one. texlab writes
/// `<number> <title>`, the title being the first `{…}` after the command, trimmed.
fn unnumbered<'a>(name: &'a str, source: &str) -> Option<&'a str> {
    let command = source.strip_prefix('\\')?;
    let end = command
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(command.len());
    let (command, rest) = command.split_at(end);
    if !matches!(command, "paragraph" | "subparagraph") && !rest.starts_with('*') {
        return None;
    }
    let title = rest[rest.find('{')? + 1..].trim_start();
    let (_, bare) = name.split_once(' ')?;
    let after = title.strip_prefix(bare)?;
    after.trim_start().starts_with('}').then_some(bare)
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

    fn tidied(text: &str, answer: Vec<Value>) -> Vec<String> {
        let symbols = serde_json::from_value(Value::Array(answer)).unwrap();
        names(&tidy(symbols, text, Encoding::Utf16))
    }

    /// What texlab 5.26 answers once a build has numbered "Results" 1 and "Method" 1.1.
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
            tidied(text, answer),
            [
                "1 Results [1.1 Method [Results [Method]]]",
                "Method [2024 was a year]"
            ]
        );
    }

    /// Shaped as texlab 5.26 answers: an `aligned` in a labelled `equation`, a labelled `align`,
    /// a label written inside a `split`, and a bare `\[ … \]`.
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
            tidied(text, answer),
            ["Maths [Equation (eq:sum), Equation (eq:row), Equation (eq:inner)]"]
        );
    }
}
