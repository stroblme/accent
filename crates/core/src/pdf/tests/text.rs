//! A page's text in reading order, the links a selection makes and where they land, and search.

use super::*;

#[test]
fn glyphs_are_non_empty_and_in_reading_order() {
    let Some((_d, doc)) = open_tiny() else { return };
    let glyphs = doc.page_text(0).unwrap();
    assert!(!glyphs.is_empty());
    let text: String = glyphs.iter().map(|g| g.ch).collect();
    assert!(text.contains("Hello accent"), "got {text:?}");
    // Single line of left-to-right text: boxes advance, and none is degenerate.
    let inked: Vec<_> = glyphs.iter().filter(|g| !g.ch.is_whitespace()).collect();
    assert!(inked.windows(2).all(|w| w[0].rect.left <= w[1].rect.left));
    assert!(inked.iter().all(|g| g.rect.height() > 0.0));
    // Indices are pdfium's and must line up with the vector positions we index by.
    assert!(glyphs.iter().enumerate().all(|(i, g)| g.index == i));
}

#[test]
fn selection_link_has_the_obsidian_shape() {
    let Some((_d, doc)) = open_tiny() else { return };
    let sel = Selection {
        page: 0,
        start: 0,
        end: 40,
    };
    let glyphs = doc.page_text(0).unwrap();
    let out = selection_link(&glyphs, "notes/paper.pdf", &sel);
    // Stand-in for ^\[\[.+\.pdf#page=\d+&selection=\d+,\d+,\d+,\d+\]\]$ without a regex dep.
    let body = out
        .link
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .expect("wrapped in [[ ]]");
    let (file, rest) = body.split_once("#page=").expect("#page=");
    assert!(file.ends_with(".pdf") && !file.is_empty());
    let (page, sel_part) = rest.split_once("&selection=").expect("&selection=");
    assert!(page.parse::<u32>().is_ok(), "page {page:?}");
    let nums: Vec<_> = sel_part.split(',').collect();
    assert_eq!(nums.len(), 4, "{sel_part:?}");
    assert!(
        nums.iter().all(|n| n.parse::<usize>().is_ok()),
        "{sel_part:?}"
    );

    assert!(out.text.starts_with("Hello"), "{:?}", out.text);
    assert!(!out.quads.is_empty());
}

#[test]
fn selection_quads_round_trips_selection_link() {
    let Some((_d, doc)) = open_tiny() else { return };
    let glyphs = doc.page_text(0).unwrap();
    let sel = Selection {
        page: 0,
        start: 0,
        end: 12,
    };
    let out = selection_link(&glyphs, "paper.pdf", &sel);
    let anchor = out.link.split_once('#').unwrap().1.trim_end_matches("]]");
    let (page, nums) = crate::markdown::pdf_anchor(anchor).unwrap();
    assert_eq!(page, 0);

    let (range, quads) = selection_quads(&glyphs, nums.unwrap()).unwrap();
    assert_eq!(range, 0..12);
    assert!(same_quads(&quads, &out.quads), "{quads:?} {:?}", out.quads);

    // Numbers past the page's lines are a link from another engine, not a panic.
    assert!(selection_quads(&glyphs, [99, 0, 99, 3]).is_none());
    assert!(selection_quads(&glyphs, [0, 0, 0, 0]).is_none());
}

/// A note link lands by its numbers, or failing them by the text it quotes; one pointing at
/// nothing paints nothing, and one exported into the file steps aside for the annotation.
#[test]
fn a_note_link_lands_by_its_numbers_then_its_text() {
    let Some((_d, mut doc)) = open_tiny() else {
        return;
    };
    let link = |selection: [usize; 4], alias: Option<&str>| crate::index::PdfLink {
        src_rel_path: "Note.md".to_string(),
        byte_start: 0,
        page: 0,
        selection,
        alias: alias.map(str::to_string),
    };
    let links = [
        link([0, 0, 0, 5], None),
        // Numbers from another edition of the document: the quoted text still finds it.
        link([7, 0, 7, 6], Some("accent")),
        link([7, 0, 7, 6], Some("not on the page")),
    ];
    let mut glyphs = std::collections::HashMap::new();
    let found = highlight_quads(&doc, &mut glyphs, &links);
    let on_page: Vec<usize> = found[&0].iter().map(|(_, at)| *at).collect();
    assert_eq!(on_page, [0, 1], "{found:?}");
    assert!(
        glyphs.contains_key(&0),
        "the page's text is kept for the next ask"
    );

    let exported = Highlight {
        page: 0,
        quads: found[&0][0].0.clone(),
        color: [255, 255, 0, 255],
        contents: None,
    };
    doc.add_highlights(&[exported]).unwrap();
    let after = highlight_quads(&doc, &mut glyphs, &links);
    let on_page: Vec<usize> = after[&0].iter().map(|(_, at)| *at).collect();
    assert_eq!(
        on_page,
        [1],
        "the exported one is in the page's own pixels now"
    );
}

#[test]
fn link_with_alias_quotes_the_text_on_one_line() {
    let link = "[[a.pdf#page=1&selection=0,0,0,5]]";
    assert_eq!(
        link_with_alias(link, " a |b]]\n c "),
        "[[a.pdf#page=1&selection=0,0,0,5|a b c]]"
    );
    assert_eq!(link_with_alias(link, "  \n "), link);
}

#[test]
fn search_is_case_insensitive_and_empty_safe() {
    let Some((_d, doc)) = open_tiny() else { return };
    let plain = Options::default();
    let hits = doc.search(1, "second", plain).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].iter().any(|r| r.width() > 0.0 && r.height() > 0.0),
        "{:?}",
        hits[0]
    );
    assert!(doc.search(1, "", plain).unwrap().is_empty(), "empty query");
    assert!(
        doc.search(0, "second", plain).unwrap().is_empty(),
        "other page"
    );
}

/// The find bar's Match Case and Match Whole Word, over "Second page".
#[test]
fn search_matches_case_and_whole_words_when_asked() {
    let Some((_d, doc)) = open_tiny() else { return };
    let count = |query, case, word| {
        let options = Options {
            case,
            word,
            regex: false,
        };
        doc.search(1, query, options).unwrap().len()
    };
    assert_eq!(count("second", true, false), 0);
    assert_eq!(count("Second", true, false), 1);
    assert_eq!(count("Secon", false, false), 1);
    assert_eq!(count("Secon", false, true), 0);
    assert_eq!(count("second", false, true), 1);
}

#[test]
fn line_groups_split_on_a_new_line() {
    let g = |ch, left: f32, top: f32| Glyph {
        ch,
        rect: Rect {
            left,
            top,
            right: left + 5.0,
            bottom: top + 10.0,
        },
        index: 0,
    };
    let glyphs = [g('a', 0.0, 0.0), g('b', 5.0, 0.0), g('c', 0.0, 12.0)];
    assert_eq!(line_groups(&glyphs), vec![0..2, 2..3]);
    assert_eq!(item_offset(&line_groups(&glyphs), 2), (1, 0));
    assert_eq!(line_groups(&[]), Vec::<Range<usize>>::new());
}
