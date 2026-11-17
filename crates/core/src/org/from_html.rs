//! Clipboard HTML as org: what `Ctrl+V` and a drop put into an org note when another application
//! offers formatted text (DESIGN.md, Editing), written from the tree `markdown::from_html` reads.
//!
//! Headings are stars, emphasis org's markers (`*bold*`, `/italic/`, `+struck+`, `_underlined_`,
//! `^{sup}`, `_{sub}`), code `=code=`, links `[[url][text]]`, quotes and code `#+begin_quote` and
//! `#+begin_src` blocks, and tables org's. Text that would read as markup is broken with the
//! zero-width space org's manual escapes with ([`super::ZWSP`]).

use super::page::object;
use super::{ZWSP, link};
use crate::markdown::{Syntax, convert};

/// The org `html` reads as, or `None` where it carries no formatting worth keeping
/// ([`crate::markdown::formatted`]), so the paste takes the plain text instead.
pub fn from_html(html: &str) -> Option<String> {
    convert::<Org>(html)
}

#[derive(Default)]
struct Org;

impl Syntax for Org {
    const MARKS: [(&'static str, &'static str); 6] = [
        ("_", "_"),
        ("^{", "}"),
        ("_{", "}"),
        ("+", "+"),
        ("*", "*"),
        ("/", "/"),
    ];
    /// `\\` ending the line.
    const BREAK: &'static str = "\\\\\n";
    /// A row is one line, so a cell's lines run on.
    const CELL_BREAK: &'static str = " ";
    const RULE: &'static str = "-----";
    /// A heading's stars open the line, which a list item's or a quote's lines do not.
    const NESTED_HEADINGS: bool = false;

    fn heading(level: usize, text: &str) -> String {
        format!("{} {text}", "*".repeat(level))
    }

    fn quote(blocks: &str) -> String {
        format!("#+begin_quote\n{blocks}\n#+end_quote")
    }

    /// A line opening with `*` or `#+`, after commas or none, escaped with a comma, as org escapes
    /// it (`org-escape-code-in-string`).
    fn code_block(lang: &str, code: &str) -> String {
        let lines: Vec<String> = (code.lines())
            .map(|line| {
                let lead = line.len() - line.trim_start_matches([' ', '\t']).len();
                let rest = line[lead..].trim_start_matches(',');
                match rest.starts_with('*') || rest.starts_with("#+") {
                    true => format!("{},{}", &line[..lead], &line[lead..]),
                    false => line.to_string(),
                }
            })
            .collect();
        let lang = match lang {
            "" => String::new(),
            lang => format!(" {lang}"),
        };
        format!("#+begin_src{lang}\n{}\n#+end_src", lines.join("\n"))
    }

    /// `=code=`, or `~code~` where it holds an `=`, which could close it early.
    fn code(code: &str) -> String {
        match code.contains('=') && !code.contains('~') {
            true => format!("~{code}~"),
            false => format!("={code}="),
        }
    }

    /// `[[href][text]]`, or `[[href]]` for an address showing itself.
    fn link(href: &str, text: &str, shows_address: bool) -> String {
        link(href, (!shows_address).then_some(text))
    }

    /// `[[src]]`, which org shows as the picture, or in a link's text the address alone, as org
    /// writes a picture that links (`[[url][https://…/picture.png]]`). Org has no place for `alt`.
    fn image(src: &str, _: &str, in_link: bool) -> String {
        match in_link {
            true => src.to_string(),
            false => link(src, None),
        }
    }

    fn tick(ticked: bool) -> &'static str {
        match ticked {
            true => "[X] ",
            false => "[ ] ",
        }
    }

    fn table_rule(width: usize) -> String {
        format!("|{}|", vec!["---"; width].join("+"))
    }

    /// The blocks on one line, a pipe written as org's `\vert{}`, which no cell ends at.
    fn cell(blocks: &[String]) -> String {
        blocks.join(" ").replace('\n', " ").replace('|', "\\vert{}")
    }

    /// A zero-width space starts a line that would open a block, and follows the first character
    /// of anything org's reader would take for markup (`super::page::object`), where it breaks
    /// the markup in Emacs and in accent alike: `[​[x]]`, `*​x*`, `$​x$`.
    fn escape(out: &mut String, text: &str) {
        let line = out.rfind('\n').map_or(0, |n| n + 1);
        if line == out.len() && opens_line(text) {
            out.push(ZWSP);
        }
        let mut at = out.len();
        out.push_str(text);
        while let Some(c) = out[at..].chars().next() {
            at += c.len_utf8();
            let end = out.len();
            if object(out, line, at - c.len_utf8(), end, end).is_some() {
                out.insert(at, ZWSP);
                at += ZWSP.len_utf8();
            }
        }
    }
}

/// Whether a line opening with `text` would read as more than text: a heading or a `*` bullet, a
/// list item, a rule, a comment or a keyword, a fixed-width line or a drawer, or a table row.
fn opens_line(text: &str) -> bool {
    let word = |rest: &str| rest.is_empty() || rest.starts_with(char::is_whitespace);
    let rule = text.trim_end().len() >= 5 && text.trim_end().trim_start_matches('-').is_empty();
    match text.chars().next() {
        Some('*') => word(text.trim_start_matches('*')),
        Some('#') => word(&text[1..]) || text[1..].starts_with('+'),
        Some(':' | '|') => true,
        Some('-' | '+') => word(&text[1..]) || rule,
        _ => {
            let rest = text.trim_start_matches(|c: char| c.is_ascii_digit());
            rest.len() < text.len() && rest.strip_prefix(['.', ')']).is_some_and(word)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::from_html;
    use crate::markdown::shown;
    use crate::org::to_html;

    /// `html` converts to `org`, which an org note shows as the text `html` showed.
    fn converts(html: &str, org: &str) -> String {
        let got = from_html(html).unwrap_or_else(|| panic!("no org for {html}"));
        assert_eq!(got, org, "{html}");
        let page = to_html(&got);
        assert_eq!(shown(&page), shown(html), "{page}");
        page
    }

    #[test]
    fn headings_are_stars() {
        let page = converts(
            "<h1>Title</h1><h3>Sub <i>it</i></h3><p>text</p>",
            "* Title\n\n*** Sub /it/\n\ntext",
        );
        assert!(page.contains("<h1") && page.contains("<h3"), "{page}");
    }

    /// A list item or a quote holds no heading: its text is bold.
    #[test]
    fn a_heading_in_a_list_is_bold() {
        converts("<ul><li><h2>Title</h2></li></ul>", "- *Title*");
    }

    #[test]
    fn emphasis_is_org_markers() {
        let page = converts(
            "<p><b>bold</b>, <i>italic</i>, <s>struck</s>, <u>under</u>, H<sub>2</sub>O and x<sup>2</sup>.</p>",
            "*bold*, /italic/, +struck+, _under_, H_{2}O and x^{2}.",
        );
        for tag in ["<strong>", "<em>", "<del>", "<u>", "<sub>", "<sup>"] {
            assert!(page.contains(tag), "{tag} in {page}");
        }
    }

    #[test]
    fn code_is_verbatim() {
        let page = converts(
            "<p>run <code>ls -l</code> or <code>a=1</code></p>",
            "run =ls -l= or ~a=1~",
        );
        assert_eq!(page.matches("<code>").count(), 2, "{page}");
    }

    #[test]
    fn links_and_pictures() {
        let page = converts(
            "see <a href='https://a.org/x'>the <b>page</b></a>, <a href='https://a.org/y'>https://a.org/y</a>",
            "see [[https://a.org/x][the *page*]], [[https://a.org/y]]",
        );
        assert!(page.contains("href=\"https://a.org/x\""), "{page}");
        assert_eq!(
            from_html(
                "<b>b</b> <img src='https://a.org/c.png' alt='c'> <a href='https://a.org'><img src='https://a.org/d.png'></a>"
            )
            .as_deref(),
            Some("*b* [[https://a.org/c.png]] [[https://a.org][https://a.org/d.png]]")
        );
        // A link alone that shows its own address, as Copy Link copies one, is that address.
        assert_eq!(
            from_html("<a href='https://a.org/x'>https://a.org/x</a>"),
            None
        );
    }

    #[test]
    fn lists_nest_and_tick() {
        converts(
            "<ul><li>one</li><li>two<ul><li><i>deep</i></li></ul></li></ul><ol start='3'><li>three</li><li>four</li></ol>",
            "- one\n- two\n  - /deep/\n\n3. three\n4. four",
        );
        let page = converts(
            "<ul><li><input type=checkbox checked> done</li><li><input type=checkbox> open</li></ul>",
            "- [X] done\n- [ ] open",
        );
        assert_eq!(page.matches("checkbox").count(), 2, "{page}");
    }

    #[test]
    fn a_quote_is_a_quote_block() {
        let page = converts(
            "<blockquote><p>Quoted <b>text</b>.</p><p>Second.</p></blockquote>",
            "#+begin_quote\nQuoted *text*.\n\nSecond.\n#+end_quote",
        );
        assert!(page.contains("<blockquote>"), "{page}");
    }

    /// A line of code that org would read as a heading or a keyword is escaped with a comma, which
    /// the page takes off again.
    #[test]
    fn code_is_a_source_block() {
        let page = converts(
            "<p>Run:</p><pre><code class='language-python'>x = 1\n* star\n  #+key</code></pre>",
            "Run:\n\n#+begin_src python\nx = 1\n,* star\n  ,#+key\n#+end_src",
        );
        assert!(
            page.contains("<pre>") && page.contains("* star\n"),
            "{page}"
        );
        // A line escaped already gets one comma more, as org adds one.
        assert_eq!(
            from_html("<p>Run:</p><pre>,* kept</pre>").as_deref(),
            Some("Run:\n\n#+begin_src\n,,* kept\n#+end_src")
        );
    }

    #[test]
    fn a_table_is_an_org_table() {
        let page = converts(
            "<table><tr><th>Name</th><th>Value</th></tr><tr><td><b>a</b></td><td>1<br>2</td></tr></table>",
            "| Name | Value |\n|---+---|\n| *a* | 1 2 |",
        );
        assert!(page.contains("<th>") && page.contains("<td>"), "{page}");
        // A pipe would end the cell.
        assert_eq!(
            from_html("<table><tr><th>a | b</th><th>c</th></tr></table>").as_deref(),
            Some("| a \\vert{} b | c |\n|---+---|")
        );
    }

    #[test]
    fn rules_and_breaks() {
        let page = converts(
            "<p>a</p><hr><p><b>b</b><br>c</p>",
            "a\n\n-----\n\n*b*\\\\\nc",
        );
        assert!(page.contains("<hr") && page.contains("<br"), "{page}");
    }

    /// Text that org would read as markup is broken with a zero-width space, so the page shows it
    /// as written: no emphasis, link, formula, script, footnote or target, and no block.
    #[test]
    fn text_is_escaped_where_it_would_be_markup() {
        let html = "<p><b>b</b> /usr/bin/ *x* =v= [[y]] [fn:1] &lt;&lt;t&gt;&gt; $x$ a_{1} \\(z\\) 2 * 3 / 4, $5 and $10, snake_case</p>";
        let page = converts(
            html,
            "*b* /\u{200B}usr/bin/ *\u{200B}x* =\u{200B}v= [\u{200B}[y]] [\u{200B}fn:1] <\u{200B}<t>> $\u{200B}x$ a_\u{200B}{1} \\\u{200B}(z\\) 2 * 3 / 4, $5 and $10, snake_case",
        );
        assert_eq!(page.matches("<strong>").count(), 1, "{page}");
        for markup in ["<em>", "<code>", "<a ", "<sub>", "math", "footnote"] {
            assert!(!page.contains(markup), "{markup} in {page}");
        }
        let blocks = "<p>* a</p><p>- b</p><p>+ c</p><p>1. d</p><p>2) e</p><p># f</p><p>#+g</p><p>| h</p><p>: i</p><p>-----</p><p><b>j</b></p>";
        let page = converts(
            blocks,
            "\u{200B}* a\n\n\u{200B}- b\n\n\u{200B}+ c\n\n\u{200B}1. d\n\n\u{200B}2) e\n\n\u{200B}# f\n\n\u{200B}#+g\n\n\u{200B}| h\n\n\u{200B}: i\n\n\u{200B}-----\n\n*j*",
        );
        for block in ["<h1", "<li", "<table", "<hr", "<pre"] {
            assert!(!page.contains(block), "{block} in {page}");
        }
        // What only looks like markup stays as it is.
        assert_eq!(
            from_html("<p><b>b</b> #tag -x 1.5 *, a/b/c</p>").as_deref(),
            Some("*b* #tag -x 1.5 *, a/b/c")
        );
    }

    /// Google Docs styles every run inline, nests a list straight in its parent list and wraps the
    /// copy in a `<b>` that is not bold.
    #[test]
    fn google_docs_reads_as_org() {
        let run = |style: &str, text: &str| {
            format!(r#"<span style="font-size:11pt;{style}white-space:pre-wrap;">{text}</span>"#)
        };
        let doc = format!(
            r#"<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-1"><h1 dir="ltr">{}</h1><p dir="ltr">{}{}</p><ul><li dir="ltr"><p dir="ltr">{}</p></li><ul><li dir="ltr"><p dir="ltr">{}</p></li></ul></ul></b>"#,
            run("font-weight:400;", "Title"),
            run("font-weight:700;", "Bold"),
            run("font-weight:400;font-style:italic;", " and italic"),
            run("font-weight:400;", "First"),
            run("font-weight:400;", "Nested"),
        );
        converts(
            &doc,
            "* Title\n\n*Bold* /and italic/\n\n- First\n  - Nested",
        );
    }
}
