//! The files the vault holds as written: org files, a note to print, notebooks and an SVG.

/// An org file's fixture: TODO keywords set by the file, tags, a title, its three kinds of link
/// and a source block and a drawer, which hold none; then what its note look styles (emphasis,
/// lists and checkboxes, a comment, a fixed-width line, a table).
pub(super) const OUTLINE_ORG: &str = "#+TITLE: Org outline\n#+FILETAGS: :org:\n#+TODO: TODO NEXT | DONE\n\n\
The Outline, folds and links of an org file: [[file:Real.md][the real note]], \
[[./Figures.md::*Figures]] and [[*Tasks]].\n\n\
* Tasks :todo:\n** NEXT [#A] Read the figures\n** DONE Index this file\n\
* Code\n#+begin_src python\ndef f():\n    return [[1]]\n#+end_src\n\
* Drawers\n:PROPERTIES:\n:ID: 0f6c9e1c\n:END:\n<<here>> is where [[here]] lands.\n\
* Look\nProse with *bold*, /italic/, +struck+, =verbatim=, ~code~ and _underline_, and \
<https://orgmode.org>.\n- [X] a done box\n- [-] one under way\n- [ ] an open one\n\
  + nested, and 1) counted\n# a comment\n: fixed width\n| a | b |\n|---+---|\n| 1 | 2 |\n";

/// A second org fixture for what an org file's page shows, which `OUTLINE_ORG` leaves to its
/// note look: inline markup and formulas, footnotes, every kind of block, a table out of line for
/// Tab to line up, the file links' search options, a planning line and property drawer, a
/// `COMMENT` heading and a saved search, with the `#+KEY:` lines, drawers and comments the page
/// leaves out.
pub(super) const PREVIEW_ORG: &str = r##"#+TITLE: Org preview
#+FILETAGS: :org:sample:
#+TODO: TODO NEXT | DONE

What an Org file shows on its page, beside [[file:Outline.org][the outline file]].

* Text :text:
Prose with *bold*, /italic/, _underline_, +struck+, =verbatim=, ~code~, H_{2}O and x^{2}, while snake_case stays.
A line ending in two backslashes breaks\\
here. Inline maths \(e^{i\pi} + 1 = 0\) and $a^2 + b^2 = c^2$, though $5 and $10 are prices.

\[
\int_0^1 x^2 \, dx = \frac{1}{3}
\]

\begin{align*}
a &= b + c \\
d &= e
\end{align*}

Footnotes are numbered and defined after the page,[fn::an inline one with /emphasis/] one by label[fn:note] too.

[fn:note] Defined where it is named.

* Lists
- [X] a done box
- [-] one under way
  + nested
- [ ] an open one

Counted:
1. first
2. second

Described:
- term :: its description

* Blocks
#+begin_quote
A quoted /line/.
#+end_quote

#+begin_verse
Roses are red,
  violets are blue.
#+end_verse

#+begin_center
A centred *paragraph*.
#+end_center

: a fixed-width line

#+begin_example
raw *text*, left as written
#+end_example

#+begin_src python
def f():
    return [[1]]
#+end_src

#+begin_src search limit=3
kind:pdf
#+end_src

#+begin_export html
<p><b>HTML</b> of the page</p>
#+end_export

#+begin_comment
Not on the page.
#+end_comment

* Tables
| <l>    | <r> |
| Name   | Qty |
|--------+-----|
| tea | 12 |
| coffee | 7 |

* Links
A heading, [[*Text]], a target, [[here]] (<<here>>), a custom ID, [[#plan]], a line, [[file:Outline.org::5]], a heading of another file, [[file:Outline.org::*Look][its look]], a match, [[file:Outline.org::/bold/][the first bold]], this file's custom ID, [[file:Preview.org::#plan]], a note, [[file:Real.md][Real]], one that is missing, [[file:Missing.md][a missing note]], and the web, [[https://orgmode.org][Org]] or <https://orgmode.org>.

[[file:../Attachments/figure.png]]

* TODO [#A] Plan the trip :travel:
SCHEDULED: <2026-11-10 Tue> DEADLINE: <2026-11-20 Fri>
:PROPERTIES:
:CUSTOM_ID: plan
:END:
Its priority, planning line and drawer are not on the page.
** NEXT Book the flight
** DONE Pick a date

* COMMENT Draft
Left out with its section.
** Notes to self
"##;

/// Print/export fixture covering headings, inline and display math, diagrams, long code lines,
/// tables, local and web images, and local, external and heading links. Its script must neither
/// execute nor appear in the export.
pub(super) const EXPORT_NOTE: &str = r#"# Export

Inline math, $e^{i\pi} + 1 = 0$, and a formula on its own lines:

$$
\int_0^1 x^2 \, dx = \frac{1}{3}
$$

```mermaid
graph LR
  A[Write] --> B[Print]
  B --> C[Share]
```

```rust
fn main() {
    println!("a line of code long enough that it has to wrap on paper rather than run off the page it is printed on");
}
```

| Column | Value |
|---|---|
| one | 1 |
| two | 2 |

![[Attachments/figure.png]]

![[Attachments/figure.svg]]

![from the web](https://www.gnome.org/wp-content/themes/gnome-grass/images/gnome-logo.svg)

A link into the vault, [[Figures]], one out of it, [GNOME](https://www.gnome.org), and one back to [the top](#export).

<script>document.body.textContent = 'scripted';</script>
"#;

/// The notebook drill's `.ipynb`, as nbformat 5.10.4 wrote it: a white-background PNG, an SVG, a
/// pandas MultiIndex table, scripted HTML beside its text, LaTeX, JSON, a progress bar's `\r`
/// stream, an ANSI traceback, an attachment and a raw cell.
pub(super) const ANALYSIS_IPYNB: &str =
    include_str!("../../tests/fixtures/notebook/analysis.ipynb");

/// The notebook drill's marimo notebook, written by hand as marimo's `codegen.py` writes one
/// (marimo is not installed here): a setup block, a hidden markdown cell, `@app.function`, a
/// `def` line and a `return` over several lines, and an unparsable cell.
pub(super) const EXPLORE_PY: &str = include_str!("../../tests/fixtures/notebook/explore.py");

/// SVG fixture with black strokes and a label on a transparent canvas.
pub(super) const FIGURE_SVG: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="320" height="200" viewBox="0 0 320 200">
  <g fill="none" stroke="#000" stroke-width="2">
    <path d="M40 20V170H300"/>
    <path d="M40 150C100 40 180 40 290 120"/>
    <rect x="200" y="30" width="80" height="40" rx="6"/>
  </g>
  <text x="240" y="55" font-family="sans-serif" font-size="14" text-anchor="middle">label</text>
</svg>
"##;
