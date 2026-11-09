//! The `search` module's tests.

use super::*;

#[test]
fn a_query_shorter_than_the_grace_period_never_draws_a_bar() {
    // A requery nobody asked for is a few milliseconds on a warm index ranked and some 60 ms
    // as an exact scan; the wait has to be long enough to cover one and short enough that a
    // real query still reports itself.
    assert!((100..=300).contains(&(PULSE * SHOW_AFTER).as_millis()));
}

#[test]
fn snippet_escapes_before_marking_up() {
    // `<` inside the note must survive as text, not as the start of a tag.
    let out = snippet_markup("a «b» < c & d");
    assert_eq!(out, "a <b>b</b> &lt; c &amp; d");
    assert!(pango::parse_markup(&out, '\u{0}').is_ok());
}

#[test]
fn a_snippet_is_one_paragraph_however_the_note_was_wrapped() {
    // `lines(2)` on the row label is counted per paragraph, so a newline here is a row that
    // grows without limit.
    let out = snippet_markup("---\ntitle: x\n---\n\nthe «body»");
    assert!(!out.contains('\n'), "{out:?}");
    assert_eq!(out, "--- title: x --- the <b>body</b>");
    assert_eq!(snippet_markup("  padded  "), "padded");
}

#[test]
fn snippet_markup_stays_valid_when_markers_are_unbalanced() {
    // A note may contain guillemets of its own, so every shape has to parse.
    for s in ["«open", "close»", "«a «b»", "»«", "«<»", ""] {
        let out = snippet_markup(s);
        assert!(
            pango::parse_markup(&out, '\u{0}').is_ok(),
            "{s:?} -> {out:?}"
        );
    }
}

#[test]
fn a_row_is_named_by_its_file_and_its_folder() {
    assert_eq!(basename("notes/deep/thought.md"), "thought.md");
    assert_eq!(dir_label("notes/deep/thought.md", None), "notes/deep/");
    assert_eq!(dir_label("top.md", None), "");
    // A row that is one match says which line it is, wherever the file sits.
    assert_eq!(
        dir_label("notes/deep/thought.md", Some(12)),
        "notes/deep/ — line 12"
    );
    assert_eq!(dir_label("top.md", Some(3)), "line 3");
}

#[test]
fn a_diagrams_row_names_the_page_its_label_is_on() {
    let cell = |id: &str, label: &str| {
        format!(
            r#"<mxCell id="{id}" value="{label}" vertex="1" parent="1"><mxGeometry as="geometry"/></mxCell>"#
        )
    };
    let page = |name: &str, cells: &str| {
        format!(
            r#"<diagram name="{name}" id="{name}"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>{cells}</root></mxGraphModel></diagram>"#
        )
    };
    let xml = format!(
        "<mxfile>{}{}</mxfile>",
        page("One", &(cell("a", "Kettle") + &cell("b", "Tea"))),
        page("Two", &cell("c", "Cup"))
    );
    let (text, starts) = diagram_text(&xml).unwrap();
    assert_eq!(
        text, "Kettle\nTea\nCup",
        "the index's text, a line per label"
    );
    let names: Vec<&str> = starts.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(names, ["One", "Two"]);
    assert_eq!(
        page_label("d/flow.drawio", &starts, 12).as_deref(),
        Some("d/ — Two")
    );
    assert_eq!(
        page_label("flow.drawio", &starts, 3).as_deref(),
        Some("One")
    );
}

#[test]
fn a_notebooks_row_names_the_cell_its_match_is_in() {
    const ANALYSIS: &str =
        include_str!("../../../../../crates/core/tests/fixtures/notebook/analysis.ipynb");
    let rel = "Notebooks/analysis.ipynb";
    let read = |_: &str| Some(ANALYSIS.to_string());
    let sources = searchable(&read, rel).expect("its cells' sources");
    let starts = notebook::read(rel, ANALYSIS).unwrap().starts();
    let at = sources.find("binomial").expect("a code cell's");
    assert_eq!(cell_label(rel, &starts, at), "Notebooks/ — cell 3");
    assert_eq!(cell_label("a.ipynb", &starts, 0), "cell 1");
}

#[test]
fn a_ranked_answer_is_one_row_per_match_and_a_tail_row() {
    let hit = |line: Option<u32>, snippet: &str, at: Range<usize>, more: usize| SearchHit {
        rel_path: "notes/a.md".into(),
        title: None,
        snippet: snippet.into(),
        at: Some(at),
        line,
        page: None,
        more,
    };
    let rows = fts_rows(
        vec![
            hit(Some(2), "one «ferris» here", 4..10, 0),
            hit(Some(7), "and «ferris» again", 40..46, 3),
            SearchHit {
                rel_path: "b.md".into(),
                title: Some("Ferris".into()),
                snippet: "# «Ferris»".into(),
                at: None,
                line: None,
                page: None,
                more: 0,
            },
        ],
        "ferris",
    );
    let seen: Vec<_> = rows
        .iter()
        .map(|r| (r.name.as_str(), r.dir.as_str(), r.at.clone()))
        .collect();
    assert_eq!(
        seen,
        [
            ("a.md", "notes/ — line 2", Some(4..10)),
            ("a.md", "notes/ — line 7", Some(40..46)),
            // The tail row opens nothing: it lists the rest in its place.
            ("", "+3 more in this file", None),
            // A hit on a title alone has no line and no place in the body.
            ("b.md", "", None),
        ]
    );
    assert_eq!(rows[0].snippet, "one <b>ferris</b> here");
    // It picks up where the last listed occurrence ends.
    let more = rows[2].more.as_ref().expect("a tail row");
    assert!(matches!(&more.how, Rest::Phrase(q) if q == "ferris"));
    assert_eq!(more.from, 46);
    assert!(rows.iter().filter(|r| r.more.is_some()).count() == 1);
}

/// An opened tail row becomes the rows it counted, in the shape of the rows above it, and a
/// file with more than [`MORE_AT_ONCE`] of them leaves a tail row for the rest.
#[test]
fn an_opened_tail_row_lists_the_rest_of_its_file() {
    let re = search::pattern("ferris", Options::default()).unwrap();
    let body = "ferris\n".repeat(7);
    let (listed, _) = accent_core::index::Index::matches_from("a.md", None, &body, &re, 0, 5);
    let rows = grep_rows(listed, &re, Some("crab"), true, "teal");
    let tail = rows
        .last()
        .and_then(|r| r.more.clone())
        .expect("a tail row");
    assert_eq!(
        rows.last().map(|r| r.dir.as_str()),
        Some("+2 more in this file")
    );

    let rest = rest_rows("a.md", &body, &tail, "teal");
    let seen: Vec<_> = rest
        .iter()
        .map(|r| (r.dir.as_str(), r.at.clone()))
        .collect();
    assert_eq!(seen, [("line 6", Some(35..41)), ("line 7", Some(42..48))]);
    // The replacement the rows above preview, previewed on these too.
    assert!(
        rest[0].snippet.contains("<s>ferris</s>"),
        "{}",
        rest[0].snippet
    );
    assert!(rest.iter().all(|r| r.more.is_none()));

    let body = "ferris\n".repeat(5 + MORE_AT_ONCE + 3);
    let rest = rest_rows("a.md", &body, &tail, "teal");
    assert_eq!(rest.len(), MORE_AT_ONCE + 1);
    assert_eq!(
        rest.last().map(|r| r.dir.as_str()),
        Some("+3 more in this file")
    );

    let rest = rest_rows("a.md", "no match left", &tail, "teal");
    assert!(rest.is_empty());

    // Asked again, the question lists the file open, from its text read with the answer.
    let bodies = Bodies::from([("a.md".to_string(), "ferris\n".repeat(7))]);
    let opened = open_tails(rows, &bodies, "teal");
    assert_eq!(opened.len(), 7);
    assert!(opened.iter().all(|r| r.more.is_none()));
}

#[test]
fn a_pdfs_row_names_its_page_opens_it_and_promises_no_replacement() {
    let hit = SearchHit {
        rel_path: "papers/a.pdf".into(),
        title: None,
        snippet: "the «Zebrafish» count".into(),
        at: Some(40..49),
        line: None,
        page: Some(12),
        more: 0,
    };
    let rows = fts_rows(vec![hit], "zebrafish");
    assert_eq!(rows[0].dir, "papers/ — p. 12");
    assert_eq!(rows[0].page, Some((11, "Zebrafish".to_string())));

    let re = search::pattern("zebra", Options::default()).unwrap();
    let m = Match {
        rel_path: "a.pdf".into(),
        title: None,
        line: 3,
        line_text: "a zebra".into(),
        range: 2..7,
        offset: 90,
        page: Some(2),
        more: 0,
    };
    let rows = grep_rows(vec![m], &re, Some("horse"), true, "teal");
    assert_eq!(rows[0].dir, "p. 2");
    assert_eq!(rows[0].page, Some((1, "zebra".to_string())));
    assert_eq!(rows[0].snippet, "a <b>zebra</b>");
}

#[test]
fn a_grep_row_marks_the_match_and_then_the_replacement() {
    // A colour name, not a hex literal: DESIGN.md's pre-flight grep allows neither outside
    // `theme.rs`, and pango parses both.
    let plain = match_markup("a <b> c", 2..5, None, "teal");
    assert_eq!(plain, "a <b>&lt;b&gt;</b> c");
    assert!(pango::parse_markup(&plain, '\u{0}').is_ok());

    let replaced = match_markup("a <b> c", 2..5, Some("&x"), "teal");
    assert!(replaced.contains("<s>&lt;b&gt;</s>"), "{replaced}");
    assert!(replaced.contains(">&amp;x</span>"), "{replaced}");
    assert!(pango::parse_markup(&replaced, '\u{0}').is_ok());
}

#[test]
fn the_slower_halves_wait_for_the_typing_to_stop() {
    assert_eq!((DEBOUNCE + WIDEN_AFTER).as_millis(), 400);
}

#[test]
fn walked_rows_come_under_a_heading_that_opens_nothing() {
    let re = search::pattern("zorblat", Options::default()).unwrap();
    assert!(walked_rows(Vec::new(), &re, "teal").is_empty());
    let walked = vec![Match {
        rel_path: "node_modules/dep.js".into(),
        title: None,
        line: 1,
        line_text: "// zorblat".into(),
        range: 3..10,
        offset: 3,
        page: None,
        more: 0,
    }];
    let rows = walked_rows(walked, &re, "teal");
    let seen: Vec<_> = rows
        .iter()
        .map(|r| (r.rel_path.as_str(), r.name.as_str()))
        .collect();
    assert_eq!(seen, [("", NOT_INDEXED), ("node_modules/dep.js", "dep.js")]);
    assert_eq!(rows[1].snippet, "// <b>zorblat</b>");
}

#[test]
fn mid_word_rows_come_under_a_heading_of_their_own() {
    assert!(mid_word_rows(Vec::new(), "afe").is_empty());
    let hit = SearchHit {
        rel_path: "menu.md".into(),
        title: None,
        snippet: "Uni«café»".into(),
        at: Some(3..8),
        line: Some(1),
        page: None,
        more: 0,
    };
    let rows = mid_word_rows(vec![hit], "afe");
    let seen: Vec<_> = rows
        .iter()
        .map(|r| (r.rel_path.as_str(), r.name.as_str()))
        .collect();
    assert_eq!(seen, [("", INSIDE_WORDS), ("menu.md", "menu.md")]);
}
