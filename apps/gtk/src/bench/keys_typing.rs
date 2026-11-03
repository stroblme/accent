//! Keyboard drills for what a press types in tables, code, lists and the completion popup.

use super::keys::at;
use super::*;
use crate::completion;
use accent_api::{Completion, Kind, Pos, Range, TextEdit};
use accent_core::note::Syntax;

/// A note's Tab, Shift+Tab and Return in a table, through `typing::on_key` as a press in a note
/// reaches it: the table lined up, the caret walking its cells, a row added past the last and an
/// empty last row ending it, each one undo step. Prints the text and the caret's line and column.
pub(super) fn bench_table(view: &multicaret::View) {
    let buffer = view.buffer();
    let none = gdk::ModifierType::empty();
    let press = |step: &str, syntax, key| {
        let answer = typing::on_key(view.upcast_ref(), syntax, key, none);
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        let at = editor::caret(&buffer);
        println!(
            "bench table_{step} {text:?} ({}, {}) {answer:?}",
            at.line(),
            at.line_offset()
        );
    };
    view.clear_carets();
    buffer.set_text("| a | bb |\n|:-|-:|\n| ccc | d |");
    buffer.place_cursor(&at(&buffer, 2, 3));
    let markdown = Syntax::Markdown;
    press("tab", markdown, gdk::Key::Tab);
    press("tab_adds_row", markdown, gdk::Key::Tab);
    press("back_tab", markdown, gdk::Key::ISO_Left_Tab);
    buffer.place_cursor(&at(&buffer, 3, 2));
    press("enter_leaves", markdown, gdk::Key::Return);
    buffer.undo();
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    println!("bench table_undo {text:?}");
    // A row of pipes with no delimiter row under its header is no table.
    buffer.set_text("a | b\nc | d");
    buffer.place_cursor(&at(&buffer, 0, 1));
    press("not_a_table", markdown, gdk::Key::Tab);
    // An org table is every line opening with a pipe, its hline redrawn to the widths, and Return
    // in its last row adds one above an hline closing it.
    buffer.set_text("| a | bb |\n|-\n| ccc | d |\n|-");
    buffer.place_cursor(&at(&buffer, 2, 3));
    press("org_tab", Syntax::Org, gdk::Key::Tab);
    press("org_enter_adds_row", Syntax::Org, gdk::Key::Return);
}

/// A code tab's Return and closers through [`editor::indent_key`], as a press in a code tab
/// reaches it: one level deeper after an opener, a comment after it or not, none after one in a
/// string or with no language; a pair split onto three lines, taken back by one Undo; a closer on
/// a blank line back out to its block's level; tabs where the view wants tabs.
pub(super) fn bench_indent(view: &multicaret::View) {
    use sourceview5::prelude::BufferExt as _;
    let buffer = view.buffer();
    let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() else {
        return;
    };
    let text = || buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    let press = |step: &str, key| {
        let answer = editor::indent_key(view.upcast_ref(), key, gdk::ModifierType::empty());
        let at = editor::caret(&buffer);
        println!(
            "bench indent_{step} {:?} ({}, {}) {answer:?}",
            text(),
            at.line(),
            at.line_offset()
        );
    };
    let set = |language: Option<&str>, code: &str| {
        let language = language.and_then(|id| sourceview5::LanguageManager::default().language(id));
        source.set_language(language.as_ref());
        buffer.set_text(code);
        buffer.place_cursor(&buffer.end_iter());
    };
    view.clear_carets();
    view.set_tab_width(4);
    view.set_insert_spaces_instead_of_tabs(true);
    set(Some("python3"), "def f():  # x");
    press("colon", gdk::Key::Return);
    set(Some("python3"), "    x = \"(\"");
    press("string", gdk::Key::Return);
    set(None, "Notes:");
    press("no_language", gdk::Key::Return);
    set(Some("rust"), "fn f() {}");
    buffer.place_cursor(&at(&buffer, 0, 8));
    press("pair", gdk::Key::Return);
    buffer.undo();
    println!("bench indent_pair_undo {:?}", text());
    set(Some("rust"), "fn f() {\n    a();\n    ");
    press("closer", gdk::Key::braceright);
    set(Some("rust"), "    if x {\n        ");
    press("closer_empty_block", gdk::Key::braceright);
    set(Some("rust"), "fn f() {\n");
    press("closer_already_out", gdk::Key::braceright);
    view.set_insert_spaces_instead_of_tabs(false);
    set(Some("rust"), "\tg(");
    press("tabs", gdk::Key::Return);
    view.set_insert_spaces_instead_of_tabs(true);
    // Insert Line Below from the start of the line takes the same rule.
    set(Some("python3"), "def f():  # x");
    buffer.place_cursor(&buffer.start_iter());
    editor::newline_below(view.upcast_ref());
    let at = editor::caret(&buffer);
    println!(
        "bench indent_line_below {:?} ({}, {})",
        text(),
        at.line(),
        at.line_offset()
    );
    source.set_language(None);
}

/// A code tab's brackets through [`typing::on_code_key`]: paired before the line's end, stepped
/// over (after a string too), taken back by Backspace and put around a selection; left alone
/// before a word, in a comment, in a string and for a quote.
pub(super) fn bench_code_pairs(view: &multicaret::View) {
    use sourceview5::prelude::BufferExt as _;
    let buffer = view.buffer();
    let Some(source) = buffer.downcast_ref::<sourceview5::Buffer>() else {
        return;
    };
    let press = |step: &str, key| {
        let answer = typing::on_code_key(view.upcast_ref(), key, gdk::ModifierType::empty());
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        let at = editor::caret(&buffer);
        println!(
            "bench pair_{step} {text:?} ({}, {}) {answer:?}",
            at.line(),
            at.line_offset()
        );
    };
    let set = |code: &str, caret: i32| {
        buffer.set_text(code);
        buffer.place_cursor(&buffer.iter_at_offset(caret));
    };
    view.clear_carets();
    let language = sourceview5::LanguageManager::default().language("rust");
    source.set_language(language.as_ref());
    set("f", 1);
    press("open", gdk::Key::parenleft);
    press("step_over", gdk::Key::parenright);
    set("g()", 2);
    press("backspace", gdk::Key::BackSpace);
    set("x", 0);
    press("before_word", gdk::Key::bracketleft);
    set("// a", 4);
    press("comment", gdk::Key::parenleft);
    set("let s = \"a", 10);
    press("string", gdk::Key::parenleft);
    set("f(\"a\")", 5);
    press("step_over_after_string", gdk::Key::parenright);
    set("a", 0);
    buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
    press("wrap", gdk::Key::bracketleft);
    set("x", 1);
    press("quote", gdk::Key::quotedbl);
    source.set_language(None);
}

/// The completion popup's keys, on a session of the drill's own answering with three fixed
/// items wherever it is asked: what opens, what typing narrows to, the arrows and the page keys
/// walking the rows, Tab accepting a snippet with an import as one undo step, Return with nothing
/// selected left to the editor, Escape kept, a word nothing matches closing it, and a column of
/// carets getting every key the popup does not take. Prints the rows, the selection, the text and
/// what each press answered.
pub(super) async fn bench_popup_keys(view: &multicaret::View) {
    let buffer = view.buffer();
    let none = gdk::ModifierType::empty();
    let at = Pos::default();
    let item = |label: &str, insert: &str, snippet: bool| Completion {
        label: label.to_string(),
        kind: Kind::Function,
        detail: None,
        doc: None,
        filter: None,
        insert: insert.to_string(),
        is_snippet: snippet,
        replace: Range { start: at, end: at },
        extra_edits: Vec::new(),
        resolve: None,
        preselect: false,
        commit: Vec::new(),
    };
    let mut alpha = item("alpha", "alpha($1)", true);
    alpha.extra_edits.push(TextEdit {
        range: Range { start: at, end: at },
        text: "use x;\n".to_string(),
    });
    let items = vec![
        alpha,
        item("alphabet", "alphabet", false),
        item("beta", "beta", false),
    ];
    let session = completion::Session::new(view.upcast_ref(), Box::new(completion::Fixed(items)));
    view.set_enable_snippets(true);
    let text = || {
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string()
    };
    let settle = || glib::timeout_future(Duration::from_millis(100));
    let open = |typed: &str| {
        view.clear_carets();
        buffer.set_text(typed);
        buffer.place_cursor(&buffer.start_iter());
        session.ask(None);
    };

    open("");
    settle().await;
    println!(
        "bench popup_open {:?} shown={}",
        session.rows().0,
        session.is_shown()
    );
    buffer.insert_at_cursor("a");
    buffer.insert_at_cursor("l");
    settle().await;
    println!("bench popup_filter {:?}", session.rows().0);
    for (step, key) in [
        ("down", gdk::Key::Down),
        ("page_down", gdk::Key::Page_Down),
        ("up", gdk::Key::Up),
    ] {
        let answer = session.press(key, none);
        println!("bench popup_{step} {answer:?} {:?}", session.rows().1);
    }
    let answer = session.press(gdk::Key::Tab, none);
    println!(
        "bench popup_tab {answer:?} {:?} shown={}",
        text(),
        session.is_shown()
    );
    buffer.undo();
    println!("bench popup_undo {:?}", text());
    // The snippet's stops are not this drill's.
    view.set_enable_snippets(false);
    view.set_enable_snippets(true);

    open("");
    settle().await;
    let answer = session.press(gdk::Key::Return, none);
    println!(
        "bench popup_return_unselected {answer:?} shown={}",
        session.is_shown()
    );
    open("");
    settle().await;
    let answer = session.press(gdk::Key::Escape, none);
    println!("bench popup_escape {answer:?} shown={}", session.is_shown());
    open("");
    settle().await;
    buffer.insert_at_cursor("alz");
    settle().await;
    println!(
        "bench popup_nomatch {:?} shown={}",
        text(),
        session.is_shown()
    );

    // Ctrl+Space over a column: what the popup does not take goes to GTK at the primary caret.
    view.clear_carets();
    buffer.set_text("a\nb");
    buffer.place_cursor(&buffer.start_iter());
    view.add_caret(true);
    session.ask(None);
    settle().await;
    let answer = session.press(gdk::Key::x, none);
    println!(
        "bench popup_column {answer:?} shown={} carets={}",
        session.is_shown(),
        view.has_carets()
    );
    session.close();
    view.clear_carets();
    view.set_enable_snippets(false);
}

/// Return at the end of every line of the note at `rel` that opens a list, a quote or an
/// enumeration, then Tab on the line it leaves behind — both through the chain a real press goes
/// through (`editor::keys`), against what [`typing::continuation`] and [`typing::list_indent`] say
/// that line should carry on with. Prints a line per item that came out wrong and a count, so a
/// note where the marker is not carried over names the lines rather than the symptom. Then Tab on
/// lines that already have text on them.
///
/// [`bench_popup`] goes first, while the tab is still untouched: a press marks it dirty and an
/// autosave a second later would write the drill's own text into the note.
pub(super) fn bench_list(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        // The note asked for, not the first tab: a session restore may have put others first.
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        bench_popup(&app, &tab, tab.text());
    });
}

/// Return and Tab down every list line of the note, and then Tab on lines with text on them.
fn bench_lines_of(app: &Rc<App>, tab: &Rc<Tab>, original: String) {
    // What a tab is worth here: the Indent Width preference.
    println!("bench list_indent_width {}", tab.view.tab_width());
    let lines: Vec<String> = original.lines().map(str::to_string).collect();
    let syntax = match tab.flavour() {
        editor::Flavour::Note(syntax) => syntax,
        _ => Syntax::Markdown,
    };
    let (mut asked, mut wrong) = (0, 0);
    for (n, line) in lines.iter().enumerate() {
        let Some(typing::Continue::Insert(want)) = typing::continuation(line, syntax) else {
            continue;
        };
        asked += 1;
        // From the note as it is every time: each press is about the document the reader has,
        // not about what the press before it left.
        tab.set_text(&original);
        tab.buffer
            .place_cursor(&editor::line_end(&tab.buffer, n as i32));
        let none = gdk::ModifierType::empty();
        editor::press(tab, gdk::Key::Return, none);
        let carried = caret_prefix(tab);
        editor::press(tab, gdk::Key::Tab, none);
        let indented = caret_prefix(tab);
        let wanted_indent = typing::list_indent(&want).map(|step| format!("{step}{want}"));
        if carried != want || Some(&indented) != wanted_indent.as_ref() {
            wrong += 1;
            println!(
                "bench list_line {} want={want:?} carried={carried:?} indent={indented:?}",
                n + 1
            );
        }
    }
    println!("bench list_wrong {wrong} of {asked}");

    // Tab from anywhere on a list line, and the lines where it is still the view's own key. The
    // line the caret ends on and the column it ends in: the indent goes in at the head of the
    // line, so the caret keeps its place in the text.
    for (what, text, at) in [
        ("text", "- item", 4),
        ("head", "- item", 0),
        ("nested", "  - nested", 8),
        ("task", "- [x] done", 9),
        ("quote", "> quoted", 5),
        ("enumerated", "12. twelfth", 6),
        ("prose", "plain text", 5),
        ("blank", "", 0),
    ] {
        tab.set_text(text);
        tab.buffer.place_cursor(&tab.buffer.iter_at_offset(at));
        let answer = editor::press(tab, gdk::Key::Tab, gdk::ModifierType::empty());
        let caret = editor::caret(&tab.buffer);
        println!(
            "bench list_tab_{what} {:?} column={} {answer:?}",
            caret_line(tab),
            caret.line_offset()
        );
    }

    // The note back as it was: the presses above marked the tab dirty, and closing it saves.
    tab.set_text(&original);
    bench_quit(app);
}

/// The completion popup over a list item in a real tab, through the key chain a press takes
/// (`editor::keys`): Return with no row selected continues the list and puts the popup away,
/// Down and Return accept the row instead, and a view taken off screen takes the popup with it.
/// Over any note: `popup_up true`, `popup_return Stop "- " up=false`,
/// `popup_selected_return Stop "- completion" up=false` and `popup_unmapped false`.
fn bench_popup(app: &Rc<App>, tab: &Rc<Tab>, original: String) {
    // `set_text` is not an edit, so nothing here arms the autosave that would write it out.
    let staged = "completion\n- comp";
    let word = Completion {
        label: "completion".to_string(),
        kind: Kind::Text,
        detail: None,
        doc: None,
        filter: None,
        insert: "completion".to_string(),
        is_snippet: false,
        replace: Range {
            start: Pos {
                line: 1,
                character: 2,
            },
            end: Pos {
                line: 1,
                character: 6,
            },
        },
        extra_edits: Vec::new(),
        resolve: None,
        preselect: false,
        commit: Vec::new(),
    };
    let (app, tab) = (app.clone(), tab.clone());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(600)).await;
        let Some(session) = completion::session(&tab) else {
            println!("bench popup_up no_session");
            return bench_lines_of(&app, &tab, original);
        };
        let none = gdk::ModifierType::empty();
        let show = async || {
            tab.set_text(staged);
            tab.buffer.place_cursor(&tab.buffer.end_iter());
            // The view measures the new lines at its next layout: until then the staged line
            // sits where the old text's lines put it, under a long note off the bottom, and the
            // popup is not shown for a place out of sight.
            glib::timeout_future(Duration::from_millis(100)).await;
            session.show_items(vec![word.clone()]);
        };
        tab.view.grab_focus();
        show().await;
        println!("bench popup_up {}", tab.popup_shown());
        let answer = editor::press(&tab, gdk::Key::Return, none);
        println!(
            "bench popup_return {answer:?} {:?} up={}",
            caret_prefix(&tab),
            tab.popup_shown()
        );
        show().await;
        editor::press(&tab, gdk::Key::Down, none);
        let answer = editor::press(&tab, gdk::Key::Return, none);
        println!(
            "bench popup_selected_return {answer:?} {:?} up={}",
            caret_prefix(&tab),
            tab.popup_shown()
        );
        show().await;
        tab.view.set_visible(false);
        println!("bench popup_unmapped {}", tab.popup_shown());
        tab.view.set_visible(true);
        bench_lines_of(&app, &tab, original);
    });
}

/// The whole line the caret is on.
fn caret_line(tab: &Rc<Tab>) -> String {
    let caret = editor::caret(&tab.buffer);
    let mut start = caret;
    start.set_line_offset(0);
    tab.buffer
        .text(&start, &editor::line_end(&tab.buffer, caret.line()), true)
        .to_string()
}

/// The caret's line up to it: the marker a press left in front of what would be typed next.
fn caret_prefix(tab: &Rc<Tab>) -> String {
    editor::line_prefix(&tab.buffer, &editor::caret(&tab.buffer)).to_string()
}
