use super::pure::*;
use super::*;
use crate::language::PdfPages;
use accent_core::fuzzy;

/// A tag is typed in the front matter's `tags:` list as one of its items, in either form; a
/// block item is one tag whatever commas it holds, as YAML reads it.
#[test]
fn a_front_matter_tag_is_an_item_of_the_tags_list() {
    fn at(text: &str) -> Option<(&str, bool)> {
        front_tag(text, text.len()).map(|(start, flow)| (&text[start..], flow))
    }
    assert_eq!(at("---\ntags: [a, b"), Some(("b", true)));
    assert_eq!(at("---\ntags: [ab"), Some(("ab", true)));
    assert_eq!(at("---\ntags: [a]"), None, "the list is closed");
    assert_eq!(
        at("---\ntags:\n  - a\n  - People/A, Peo"),
        Some(("People/A, Peo", false))
    );
    assert_eq!(at("---\ntags:\n- a\n- b"), Some(("b", false)));
    assert_eq!(at("---\naliases:\n  - b"), None, "another key's list");
    assert_eq!(
        at("---\ntags: [a]\n---\ntags: [b"),
        None,
        "past the front matter"
    );
    assert_eq!(at("tags: [b"), None, "no front matter");
    assert_eq!(yaml_item("People/A", true), "People/A");
    assert_eq!(yaml_item("a, b", true), "\"a, b\"");
    assert_eq!(yaml_item("a, b", false), "a, b");
}

/// Tag completion lists the tags of the notes most like this one first, then the rest by use,
/// in the body and in the front matter's `tags:` list alike, where an item is written without
/// a `#`. A note like no other keeps the order by use.
#[test]
fn tag_completion_puts_the_tags_of_similar_notes_first() {
    let vault = tempfile::tempdir().unwrap();
    for (rel, text) in [
        ("q1.md", "Transmon qubit readout drifted. #hardware"),
        (
            "q2.md",
            "Transmon qubit coherence under flux noise. #hardware #lab",
        ),
        ("b1.md", "Sourdough bread wants a starter. #cooking"),
        ("m1.md", "Budget meeting. #admin"),
        ("m2.md", "Schedule meeting. #admin"),
        ("m3.md", "Budget and schedule. #admin"),
        (
            "a.md",
            "---\ntags: [lab, \n---\nThe transmon qubit lost coherence; see #\n",
        ),
        (
            "c.md",
            "---\ntags:\n  - lab\n  - \n---\nTransmon qubit readout and coherence.\n",
        ),
        ("d.md", "Zebra quokka #\n"),
    ] {
        std::fs::write(vault.path().join(rel), text).unwrap();
    }
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let first = |rel: &str, line, character| {
        let items = notes
            .completion(rel, Pos { line, character })
            .unwrap()
            .items;
        items.first().map(|i| i.insert.clone())
    };
    assert_eq!(first("a.md", 3, 40).as_deref(), Some("#hardware"));
    assert_eq!(first("a.md", 1, 12).as_deref(), Some("hardware"));
    assert_eq!(first("c.md", 3, 4).as_deref(), Some("hardware"));
    assert_eq!(first("d.md", 0, 14).as_deref(), Some("#admin"));
}

#[test]
fn context_finds_a_wikilink_or_a_tag() {
    assert_eq!(context("see [[Dee"), Some((Trigger::Wiki, 4, "Dee")));
    assert_eq!(
        context("[[Deep Work"),
        Some((Trigger::Wiki, 0, "Deep Work"))
    );
    assert_eq!(context("a [Dee"), None, "one bracket is not a wikilink");
    assert_eq!(context("[[Deep Work]] and"), None, "already closed");
    // The `#` of an anchor belongs to the link, not to a tag.
    assert_eq!(
        context("[[Note#Head"),
        Some((Trigger::Wiki, 0, "Note#Head"))
    );

    assert_eq!(
        context("note about #area/"),
        Some((Trigger::Tag, 11, "area/"))
    );
    assert_eq!(context("a #"), Some((Trigger::Tag, 2, "")), "just opened");
    assert_eq!(context("a #area and more"), None, "whitespace ends a tag");
    for head in ["# Heading", "##", "  #", "\t### "] {
        assert_eq!(context(head), None, "{head:?} is a heading marker");
    }
}

/// A fragment asked for in its middle runs on to its end: a link's target to its `]]`, a
/// destination to its `)`, a tag over its own characters. What follows a link nothing closes
/// is prose, and a tail that would open another fragment is not this one's.
#[test]
fn a_fragment_runs_past_the_caret_to_its_end() {
    type Context = fn(&str) -> Option<(Trigger, usize, &str)>;
    let whole = |line: &str, context: Context, tag: fn(char) -> bool| {
        let caret = line.find('|').unwrap();
        let line = line.replace('|', "");
        line[..fragment_end(&line, caret, context, tag)].to_string()
    };
    let md = |line| whole(line, context, markdown::is_tag_char);
    assert_eq!(md("see [[De|ep]] x"), "see [[Deep");
    assert_eq!(md("[[No|te#Head]]"), "[[Note#Head");
    assert_eq!(md("see [[De| and more"), "see [[De", "no `]]` closes it");
    assert_eq!(md("[[De|x [[y]]"), "[[De", "the tail opens another link");
    assert_eq!(md("[t](Oth|er.md#sec) x"), "[t](Other.md#sec");
    assert_eq!(md("a #ta|g, b"), "a #tag");
    assert_eq!(md("plain te|xt"), "plain te");
    let org = |line| whole(line, org_context, org::is_tag_char);
    assert_eq!(org("* Plan :wo|rk:"), "* Plan :work");
    assert_eq!(org("[[file:a|b.org]]"), "[[file:ab.org");
}

/// Asked inside a fragment, the rows are what all of it completes and replace all of it, the
/// `]]` after it too; the fragment the answer names is its text, past its `[[` or its `#`.
#[test]
fn a_completion_asked_inside_a_fragment_takes_all_of_it() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(vault.path().join("Deep.md"), "# Deep\n#deep-work\n").unwrap();
    std::fs::write(vault.path().join("a.md"), "[[Deep]]\n#deep-wo\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let ask = |line, character| notes.completion("a.md", Pos { line, character }).unwrap();
    let span = |start, end| Range {
        start: Pos {
            line: 0,
            character: start,
        },
        end: Pos {
            line: 0,
            character: end,
        },
    };
    let link = ask(0, 4);
    let deep = link.items.iter().find(|i| i.insert == "[[Deep]]").unwrap();
    assert_eq!(deep.replace, span(0, 8));
    assert_eq!(link.fragment, Some(span(2, 6)));
    let tag = ask(1, 3);
    let line = |r: Range| (r.start.character, r.end.character);
    assert!(tag.items.iter().any(|i| i.insert == "#deep-work"));
    assert!(tag.items.iter().all(|i| line(i.replace) == (0, 8)));
    assert_eq!(tag.fragment.map(line), Some((1, 8)));
}

#[test]
fn org_context_finds_a_file_link_or_a_heading_tag() {
    assert_eq!(org_context("see [[fi"), Some((Trigger::Wiki, 4, "fi")));
    assert_eq!(org_context("[[a][b]] and"), None, "already closed");
    assert_eq!(org_context("** Plan :"), Some((Trigger::Tag, 9, "")));
    assert_eq!(org_context("* Plan :wo"), Some((Trigger::Tag, 8, "wo")));
    assert_eq!(org_context("* Plan :a:b"), Some((Trigger::Tag, 10, "b")));
    assert_eq!(org_context("#+filetags: :a:"), Some((Trigger::Tag, 15, "")));
    for head in [
        "#+FILETAGS:",
        "* Meet at 10:30",
        "Note: x",
        "- item :x",
        "*bold :x",
    ] {
        assert_eq!(org_context(head), None, "{head:?} holds no tag");
    }
}

#[test]
fn org_completes_file_links_and_tags_as_org_writes_them() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir(vault.path().join("sub")).unwrap();
    std::fs::write(vault.path().join("Plan.md"), "# The plan\n#work #a/b\n").unwrap();
    std::fs::write(vault.path().join("pic.png"), "").unwrap();
    let org = "* Notes :\n[[Pl]]\n[[pi\n";
    std::fs::write(vault.path().join("sub/b.org"), org).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let ask = |line, character| {
        let items = notes
            .completion("sub/b.org", Pos { line, character })
            .unwrap()
            .items;
        items
            .into_iter()
            .map(|i| (i.insert, i.replace.end.character))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ask(1, 4),
        [("[[file:../Plan.md][The plan]]".to_string(), 6)],
        "the `]]` after the caret goes"
    );
    assert_eq!(ask(2, 4), [("[[file:../pic.png]]".to_string(), 4)]);
    assert_eq!(ask(0, 9), [("work:".to_string(), 9)], "`a/b` is no org tag");
}

#[test]
fn a_link_destination_offers_headings_and_a_line_start_tag_needs_a_letter() {
    assert_eq!(context("[A](#"), Some((Trigger::Anchor, 4, "")));
    assert_eq!(context("see [A](#Se"), Some((Trigger::Anchor, 8, "Se")));
    assert_eq!(
        context("[x](Other.md#se"),
        Some((Trigger::Anchor, 4, "se")),
        "an anchor into another note starts with its path"
    );
    // A closed link leaves the rest of the line to decide.
    assert_eq!(context("[a](#x) #ta"), Some((Trigger::Tag, 8, "ta")));
    assert_eq!(context("[[N]] #ta"), Some((Trigger::Tag, 6, "ta")));
    // A heading needs `# `, so a letter straight after a single `#` makes a tag.
    assert_eq!(context("#x"), Some((Trigger::Tag, 0, "x")));
    assert_eq!(context("##x"), None);
    assert_eq!(context("# "), None);
}

#[test]
fn an_embed_offers_files_and_a_link_destination_offers_paths() {
    assert_eq!(context("![[x"), Some((Trigger::Embed, 1, "x")));
    assert_eq!(context("see ![[sub/"), Some((Trigger::Embed, 5, "sub/")));
    assert_eq!(context("[t](Att"), Some((Trigger::Path, 4, "Att")));
    assert_eq!(context("![a]("), Some((Trigger::Path, 5, "")));
    assert_eq!(context("[t](#h"), Some((Trigger::Anchor, 4, "h")));

    // A note goes by its stem, anything else by its whole name, and both by their path.
    assert_eq!(
        link_names("sub/Beta.md"),
        ("Beta".to_string(), "sub/Beta".to_string())
    );
    assert_eq!(
        link_names("Attachments/logo.png"),
        ("logo.png".to_string(), "Attachments/logo.png".to_string())
    );
}

#[test]
fn a_hint_checks_wikilinks_by_name_and_markdown_links_by_vault_path() {
    let text = "[[Beta]] [b](../Beta%20Two.md#x) [u](https://e.com) [m](mailto:a@b.c) \
                [h](#x) [[#h]] [o](../../x.md) [r](/x.md)";
    let checked: Vec<Option<String>> = markdown::analyze(text)
        .links
        .iter()
        .map(|l| checked_target("sub/a.md", l))
        .collect();
    let want = [Some("Beta"), Some("Beta Two.md")];
    assert_eq!(checked[..2], want.map(|t| t.map(String::from)));
    // URLs, pure anchors and paths that leave the vault are not the index's to judge.
    assert!(checked[2..7].iter().all(Option::is_none), "{checked:?}");
    // A leading `/` is the vault root, as the index and the preview read it.
    assert_eq!(checked[7].as_deref(), Some("x.md"));
}

/// A note opening on its title says it once, in bold above the excerpt; a title that is not
/// the note's opening line leaves the excerpt whole.
#[test]
fn a_hover_says_the_title_once() {
    assert_eq!(
        preview("---\ntags: [x]\n---\n\n# Title\n\nBody.\n", "a.md"),
        "**Title**\n\nBody."
    );
    assert_eq!(
        preview("Intro.\n# Title\n", "a.md"),
        "**Title**\n\nIntro.\n# Title"
    );
    assert_eq!(preview("Body.\n", "sub/a.md"), "**a**\n\nBody.");
}

#[test]
fn heading_names_keep_their_order_once_each() {
    let a = markdown::analyze("# B\n## A\n#\n### B\n");
    assert_eq!(heading_names(&a.headings), ["B", "A"]);
}

/// `[[paper.pdf#` completes to the document's bookmarks, and what it inserts has to be an
/// anchor `open_target` can land on.
#[test]
fn page_links_name_a_bookmarks_page_from_one() {
    let outline = [
        ("Intro".to_string(), Some(0)),
        ("  Method  ".to_string(), Some(4)),
        ("".to_string(), Some(7)),
        ("Nowhere".to_string(), None),
    ];
    let items = page_links(&outline, "paper.pdf", Range::default());

    assert_eq!(
        items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
        ["Intro", "Method"],
        "a bookmark with no title or no page names nothing to go to"
    );
    assert_eq!(items[1].insert, "[[paper.pdf#page=5]]");
    assert_eq!(items[1].filter.as_deref(), Some("[[paper.pdf#Method"));
    // The anchor is the one the reader lands by: page 5 as written is index 4.
    assert_eq!(markdown::pdf_anchor("page=5"), Some((4, None)));
}

/// A side with no PDF reader — a host's `serve` — cannot list the bookmarks, so it answers with
/// the PDF and the link for whoever holds a copy of the file to list them from.
#[test]
fn a_pdf_nobody_here_can_read_is_left_to_the_side_with_a_copy() {
    let pages = PdfPages {
        rel: "Papers/paper.pdf".to_string(),
        note: "paper.pdf".to_string(),
        replace: Range::default(),
    };
    let unread = pages.clone().answer(None);
    assert!(unread.items.is_empty());
    assert_eq!(unread.pages.as_ref(), Some(&pages));

    let read = pages.answer(Some(vec![("Intro".to_string(), Some(0))]));
    assert_eq!(
        read.pages, None,
        "listed, so nothing is left for anyone else"
    );
    assert_eq!(read.items[0].insert, "[[paper.pdf#page=1]]");
}

/// A second `[[` to a note that is only linked to so far offers it, spelled the way the first
/// link will reach it once it is written, behind the note that is there.
#[test]
fn a_wikilink_completes_to_a_note_not_written_yet() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(vault.path().join("Other.md"), "# Other\n").unwrap();
    std::fs::write(vault.path().join("a.md"), "[[Nowhere/Other]]\n[[Oth\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let caret = Pos {
        line: 1,
        character: 5,
    };
    let items = notes.completion("a.md", caret).unwrap().items;

    let rows: Vec<(&str, Option<&str>)> = items
        .iter()
        .map(|i| (i.insert.as_str(), i.detail.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [
            ("[[Other]]", None),
            ("[[Nowhere/Other]]", Some("Nowhere/Other.md, not created"))
        ]
    );
}

/// An org file link's `::2` is a line of any file, past the last one the last, as org's own
/// goes to the end; `[[2]]` into the org file itself names a heading.
#[test]
fn an_org_file_links_digits_are_a_line() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(vault.path().join("code.py"), "a\nb\nc\n").unwrap();
    let org = "[[file:code.py::2]] [[file:code.py::99]]\n* 2\n[[2]]\n";
    std::fs::write(vault.path().join("a.org"), org).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let landed = |line, character| {
        let loc = notes
            .definition("a.org", Pos { line, character })
            .unwrap()
            .remove(0);
        (loc.path, loc.range.start.line, loc.anchor)
    };
    assert_eq!(landed(0, 3), ("code.py".into(), 1, None));
    assert_eq!(landed(0, 23), ("code.py".into(), 2, None));
    assert_eq!(landed(2, 2), ("a.org".into(), 1, None));
}

/// `#^id` goes to the block the id marks, from a wikilink or a markdown link, and one the
/// note does not have comes back as the anchor it missed; `#^` offers the note's ids.
#[test]
fn a_block_anchor_is_followed_and_completed() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(
        vault.path().join("Other.md"),
        "# Other\nFirst.\nSecond. ^blk\n",
    )
    .unwrap();
    let a = "[[Other#^blk]] [x](Other.md#^BLK) [[Other#^gone]]\n[[Other#^\n[y](Other.md#^\n";
    std::fs::write(vault.path().join("a.md"), a).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let at = |line, character| Pos { line, character };
    let landed = |pos| {
        let loc = notes.definition("a.md", pos).unwrap().remove(0);
        (loc.path, loc.range.start, loc.range.end, loc.anchor)
    };
    let block = ("Other.md".into(), at(1, 0), at(2, 12), None);
    assert_eq!(landed(at(0, 3)), block);
    assert_eq!(landed(at(0, 17)), block, "a markdown link, in any case");
    assert_eq!(
        landed(at(0, 38)),
        ("Other.md".into(), at(0, 0), at(0, 0), Some("^gone".into()))
    );

    let rows = |pos| -> Vec<(String, Option<String>, String)> {
        let items = notes.completion("a.md", pos).unwrap().items;
        items
            .into_iter()
            .map(|i| (i.label, i.detail, i.insert))
            .collect()
    };
    let row = |insert: &str| ("^blk".into(), Some("First.".into()), insert.into());
    assert_eq!(rows(at(1, 9)), [row("[[Other#^blk]]")]);
    assert_eq!(rows(at(2, 14)), [row("Other.md#^blk")]);
}

/// A note is a note by [`path::file_type`], so `.MD` and `.markdown` offer their headings and
/// preview on hover as `.md` does.
#[test]
fn a_note_answers_whatever_its_extension_case_or_spelling() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(vault.path().join("Upper.MD"), "# Alpha\nUpper body.\n").unwrap();
    std::fs::write(vault.path().join("long.markdown"), "# Beta\nLong body.\n").unwrap();
    let a = "[[Upper#\n[x](long.markdown#\n[[long]]\n";
    std::fs::write(vault.path().join("a.md"), a).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let at = |line, character| Pos { line, character };
    let labels = |pos| -> Vec<String> {
        let items = notes.completion("a.md", pos).unwrap().items;
        items.into_iter().map(|i| i.label).collect()
    };
    assert_eq!(labels(at(0, 8)), ["Alpha"]);
    assert_eq!(labels(at(1, 18)), ["Beta"]);
    let hover = notes.hover("a.md", at(2, 3)).unwrap().unwrap();
    assert!(hover.text.contains("Long body."), "{}", hover.text);
}

/// The paths a completion ranks are kept between keystrokes, and read again once the index
/// has been written: a note that arrives while the popup is up is offered at the next one.
#[test]
fn a_completion_reads_the_paths_again_once_the_index_changes() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(vault.path().join("Other.md"), "# Other\n").unwrap();
    std::fs::write(vault.path().join("a.md"), "[[Oth\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    let mut writer = Index::open(&db).unwrap();
    writer.reconcile(vault.path(), |_| {}).unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let caret = Pos {
        line: 0,
        character: 5,
    };
    let labels = || -> Vec<String> {
        let items = notes.completion("a.md", caret).unwrap().items;
        items.into_iter().map(|i| i.label).collect()
    };
    assert_eq!(labels(), ["Other"]);
    assert_eq!(labels(), ["Other"], "kept, and the same");

    std::fs::write(vault.path().join("Otherwise.md"), "# Otherwise\n").unwrap();
    writer.reconcile(vault.path(), |_| {}).unwrap();
    assert_eq!(labels(), ["Other", "Otherwise"]);
}

/// Keep the extension in completion `filter` to match the ranked path.
/// Otherwise the popup drops a valid row when typing `[[Other.md`.
#[test]
fn a_typed_extension_keeps_the_note_row() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(vault.path().join("Other.md"), "# Other\n").unwrap();
    std::fs::write(vault.path().join("a.md"), "[[Nowhere/Other]]\n[[Other.md\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let caret = Pos {
        line: 1,
        character: 10,
    };
    let items = notes.completion("a.md", caret).unwrap().items;

    let inserts: Vec<&str> = items.iter().map(|i| i.insert.as_str()).collect();
    assert_eq!(inserts, ["[[Other]]", "[[Nowhere/Other]]"]);
    // What the popup narrows every row it is handed by.
    let kept = |filter: &str| {
        fuzzy::Query::as_typed("[[Other.md", fuzzy::Corpus::Paths)
            .score(filter)
            .is_some()
    };
    for item in &items {
        let filter = item.filter.as_deref().unwrap_or(&item.label);
        assert!(kept(filter), "the popup would drop {filter:?}");
    }
}

/// A link's destination is typed as the name reads or as the link spells it, encoded, and
/// the popup keeps the row it was offered either way.
#[test]
fn a_link_destination_row_survives_the_popup_typed_either_way() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir(vault.path().join("sub")).unwrap();
    std::fs::write(vault.path().join("sub/Über Notiz.md"), "# Ü\n").unwrap();
    std::fs::write(vault.path().join("a.md"), "[t](Übe\n[t](sub/%C3%9Cb\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    for (line, character, typed) in [(0, 7, "Übe"), (1, 15, "sub/%C3%9Cb")] {
        let items = notes
            .completion("a.md", Pos { line, character })
            .unwrap()
            .items;
        assert_eq!(items[0].insert, "sub/%C3%9Cber%20Notiz.md", "{typed}");
        // What the popup narrows every row it is handed by.
        let filter = items[0].filter.as_deref().unwrap_or(&items[0].label);
        let kept = fuzzy::Query::as_typed(typed, fuzzy::Corpus::Paths)
            .score(filter)
            .is_some();
        assert!(kept, "the popup would drop {filter:?} for {typed:?}");
    }
}

/// A front matter alias is offered by its own name and writes a link to the file, spelled as
/// any `[[` completion spells it, that reads as the alias: an alias is never a link target.
#[test]
fn a_wikilink_completes_an_alias_to_its_note() {
    let vault = tempfile::tempdir().unwrap();
    std::fs::create_dir(vault.path().join("sub")).unwrap();
    std::fs::write(
        vault.path().join("sub/Real Name.md"),
        "---\naliases: [Nickname]\n---\n",
    )
    .unwrap();
    std::fs::write(vault.path().join("a.md"), "[[Nick\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();
    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let caret = Pos {
        line: 0,
        character: 6,
    };
    let items = notes.completion("a.md", caret).unwrap().items;

    let rows: Vec<(&str, &str, Option<&str>)> = items
        .iter()
        .map(|i| (i.label.as_str(), i.insert.as_str(), i.detail.as_deref()))
        .collect();
    assert_eq!(
        rows,
        [(
            "Nickname",
            "[[Real Name|Nickname]]",
            Some("sub/Real Name.md")
        )]
    );
}

/// The whole `[[paper.pdf#` path: the index resolves the name, pdfium reads the outline, and
/// what goes in is an anchor the reader can land by. Skipped where there is no libpdfium.
#[cfg(feature = "pdf")]
#[test]
fn a_wiki_anchor_into_a_pdf_offers_its_bookmarks() {
    if !accent_core::pdf::available() {
        eprintln!("skipping: no libpdfium");
        return;
    }
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(vault.path().join("paper.pdf"), bookmarked_pdf()).unwrap();
    std::fs::write(vault.path().join("a.md"), "[[paper.pdf#\n").unwrap();
    let cache = tempfile::tempdir().unwrap();
    let db = cache.path().join("i.db");
    Index::open(&db)
        .unwrap()
        .reconcile(vault.path(), |_| {})
        .unwrap();

    let notes = Notes::open_at(
        vault.path().to_path_buf(),
        &db,
        std::sync::mpsc::channel().0,
        Arc::new(Ignored::new(vault.path().to_path_buf())),
    )
    .unwrap();
    let items = notes
        .completion(
            "a.md",
            Pos {
                line: 0,
                character: 12,
            },
        )
        .unwrap()
        .items;

    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].label, "Second");
    assert_eq!(items[0].insert, "[[paper.pdf#page=2]]");
}

/// Two blank pages and one bookmark on the second, written by hand so the test needs no
/// fixture file. `crates/core/src/pdf/tests.rs` builds a fuller document the same way.
#[cfg(feature = "pdf")]
fn bookmarked_pdf() -> Vec<u8> {
    let objs = [
        "<</Type/Catalog/Pages 2 0 R/Outlines 4 0 R>>",
        "<</Type/Pages/Kids[3 0 R 5 0 R]/Count 2>>",
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>",
        "<</Type/Outlines/First 6 0 R/Last 6 0 R/Count 1>>",
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 100]>>",
        "<</Title(Second)/Parent 4 0 R/Dest[5 0 R /XYZ 0 80 0]>>",
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objs.len() + 1
    ));
    for off in &offsets {
        out.push_str(&format!("{off:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
        objs.len() + 1
    ));
    out.into_bytes()
}

#[test]
fn symbols_nest_by_level_and_span_their_section() {
    let text = "# One\ntext\n## Two\nmore\n### Three\n# Four\n";
    let a = markdown::analyze(text);
    let syms = symbols_of(text, &a.headings);

    assert_eq!(syms.len(), 2);
    assert_eq!(syms[0].name, "One");
    assert_eq!(syms[0].selection.start.line, 0);
    assert_eq!(
        syms[0].range.end.line, 4,
        "down to the last line above Four"
    );
    let two = &syms[0].children[0];
    assert_eq!(two.name, "Two");
    assert_eq!(two.children[0].name, "Three", "a deeper heading nests");
    assert_eq!(syms[1].name, "Four");
    assert_eq!(
        syms[1].range.end.line, 6,
        "the last section runs to the end, the line after the final newline included"
    );
}

#[test]
fn folds_cover_sections_fences_and_frontmatter() {
    let text = "---\ntitle: T\n---\n# One\ntext\n```rust\nfn a() {}\n```\n## Leaf\n";
    let folds = folds_of(text, &markdown::analyze(text));

    assert!(folds.contains(&Fold {
        start_line: 0,
        end_line: 2
    }));
    assert!(folds.contains(&Fold {
        start_line: 3,
        end_line: 8
    }));
    assert!(folds.contains(&Fold {
        start_line: 5,
        end_line: 7
    }));
    assert!(
        !folds.iter().any(|f| f.start_line == 8),
        "a heading with nothing under it folds nothing"
    );
}

#[test]
fn an_org_file_outlines_and_folds_as_a_note_does() {
    let text = "#+TITLE: T\n* One\ntext\n#+begin_src sh\nls\n#+end_src\n** Two :x:\n* Three\n";
    let a = Syntax::Org.analyze(text);
    let rows: Vec<(String, usize)> = symbols_of(text, &a.headings)
        .into_iter()
        .map(|s| (s.name, s.children.len()))
        .collect();
    assert_eq!(rows, [("One".to_string(), 1), ("Three".to_string(), 0)]);
    let folds: Vec<(u32, u32)> = folds_of(text, &a)
        .into_iter()
        .map(|f| (f.start_line, f.end_line))
        .collect();
    assert_eq!(folds, [(1, 6), (3, 5)]);
}

#[test]
fn path_candidates_prefer_the_shortest_path_and_tags_keep_their_order() {
    let paths: Vec<String> = ["sub/Alphabet.md", "Alpha.md", "Beta.md"]
        .map(String::from)
        .into();

    assert_eq!(
        path_candidates(&paths, "alp").0,
        ["Alpha.md", "sub/Alphabet.md"]
    );
    assert_eq!(path_candidates(&paths, "sub/").0, ["sub/Alphabet.md"]);
    assert!(path_candidates(&paths, "zzz").0.is_empty());
    // A typed extension still opens the name.
    let files: Vec<String> = ["b/xlogo.png", "a/deep/logo.png"].map(String::from).into();
    assert_eq!(
        path_candidates(&files, "logo.p").0,
        ["a/deep/logo.png", "b/xlogo.png"]
    );

    // The index counts them, so the order it hands them over in is the one to keep.
    let tags = [("alpha".to_string(), 2), ("alphabet".to_string(), 1)];
    assert_eq!(tag_candidates(&tags, "alp").0, ["alpha", "alphabet"]);
    // Not only from the start: the part of a nested tag one remembers is rarely the first.
    let nested = [("a/bc".to_string(), 1), ("other".to_string(), 1)];
    assert_eq!(tag_candidates(&nested, "bc").0, ["a/bc"]);
    assert!(tag_candidates(&tags, "z").0.is_empty());
}

/// Verify matches inside names and incomplete-list reporting, which keeps
/// the popup querying beyond the initial cap.
#[test]
fn a_query_matches_anywhere_and_a_full_list_says_it_is_not_all_of_them() {
    let paths: Vec<String> = ["Rework.md", "Projects/Groundwork.md", "Beta.md"]
        .map(String::from)
        .into();

    let (hits, more) = path_candidates(&paths, "work");
    assert_eq!(
        hits,
        ["Rework.md", "Projects/Groundwork.md"],
        "what starts with the query first, then the shortest path"
    );
    assert!(!more, "everything that matched fits");
    // Nor does it have to be contiguous, since the matcher is the palette's.
    let scattered: Vec<String> = ["a-bc.md", "Deep-Work.md"].map(String::from).into();
    assert_eq!(path_candidates(&scattered, "bc").0, ["a-bc.md"]);
    assert_eq!(path_candidates(&scattered, "dpwk").0, ["Deep-Work.md"]);

    let many: Vec<String> = (0..COMPLETIONS + 5)
        .map(|n| format!("Note{n}.md"))
        .collect();
    let (hits, more) = path_candidates(&many, "note");
    assert_eq!(hits.len(), COMPLETIONS);
    assert!(more, "the popup has to ask again as the word grows");
}
