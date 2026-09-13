//! Drills over a note's text: styling as it is typed, pastes, Ctrl+hover and occurrences.

use super::*;

/// Type a heading into the note at `rel`, at a size that styles on the keystroke and at one that
/// used to wait for the debounce, and print whether the `h1` tag is on the line *before the main
/// loop turns again*. `changed` is emitted from inside the insert, so a `true` here can only have
/// come from the synchronous path — which is the whole question this bench answers.
pub(super) fn bench_style(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        for chars in [2 * 1024, 32 * 1024] {
            bench_style_typing(&tab, chars);
        }
        bench_style_fenced(&tab);
        // A heading typed far from the caret is what the fast path deliberately leaves out: it
        // belongs to the debounced pass, and this says the pass still lands and still fixes it.
        tab.buffer.insert(&mut tab.buffer.start_iter(), "# Far\n");
        println!("bench style_far_sync {}", bench_heading_at(&tab, 0));
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            println!("bench style_debounced {}", bench_heading_at(&tab, 0));
            glib::spawn_future_local(async move {
                bench_style_paste(&tab).await;
                bench_quit(&app);
            });
        });
    });
}

/// Copy part of a styled line, paste it into a plain one, and print whether the pasted text still
/// carries a heading's or a bold's tags. GTK's own copy is rich text, and pasting it back inserts
/// the copy run by run with each run's tags applied after `changed` has re-tagged the text, so the
/// last run kept its tags: a selection ending one character into a bold word left that character
/// bold. Every paste lands mid-line, so nothing it brings can start a heading of its own. The
/// middle click and the drop are the other two ways text comes in from the buffer itself.
async fn bench_style_paste(tab: &Rc<Tab>) {
    const HEADING: &str = "# Heading here\nplain line\n";
    const BOLD: &str = "Some **bold** words\nplain line\n";
    const FOLDED: &str = "# One\nhidden body\n# Two\nplain line\n";
    for (case, text, selected, how) in [
        ("heading", HEADING, "ading he", "copy"),
        ("bold_tail", BOLD, "Some **b", "copy"),
        // The primary selection a middle click pastes is this buffer itself, tags and all.
        ("primary", BOLD, "Some **b", "primary"),
        // Across a folded section, whose tag hides text rather than styling it.
        ("primary_fold", FOLDED, "One\nhidden body\n", "primary"),
        // A drag inside the note, dropped the way the view's drop target takes it. Across a fold
        // the drag carries the visible text only, which a move then replaces the whole range with.
        ("drop", BOLD, "Some **b", "drop"),
        ("drop_fold", FOLDED, "One\nhidden body\n", "drop"),
    ] {
        tab.set_text(text);
        if text == FOLDED {
            let fold = accent_api::Fold {
                start_line: 0,
                end_line: 1,
            };
            crate::fold::fold(tab.buffer.upcast_ref(), fold);
        }
        // ASCII throughout, so a byte offset is also the character offset the buffer counts in.
        let at = |needle: &str| text.find(needle).expect("bench needle") as i32;
        let len = selected.len() as i32;
        tab.buffer.select_range(
            &tab.buffer.iter_at_offset(at(selected)),
            &tab.buffer.iter_at_offset(at(selected) + len),
        );
        let into = at("line");
        match how {
            // What a middle click runs: the selection stays, the text goes in where the click was.
            "primary" => crate::editor::paste_primary(&tab.view, &tab.buffer.iter_at_offset(into)),
            // What the drag carries is the selection's content provider; the view's drop target
            // reads it as a string and inserts that where its `gtk_drag_target` mark is.
            "drop" => {
                let stream = gio::MemoryOutputStream::new_resizable();
                let content = tab.buffer.selection_content();
                let source = content
                    .value(gtk::TextBuffer::static_type())
                    .expect("bench drag content");
                let mime = "text/plain;charset=utf-8";
                gdk::content_serialize_future(&stream, mime, &source, glib::Priority::DEFAULT)
                    .await
                    .expect("bench drag serialize");
                stream
                    .close(gio::Cancellable::NONE)
                    .expect("bench drag stream");
                let dropped = String::from_utf8_lossy(&stream.steal_as_bytes()).into_owned();
                let mark = tab.buffer.mark("gtk_drag_target").expect("bench drag mark");
                tab.buffer
                    .move_mark(&mark, &tab.buffer.iter_at_offset(into));
                let target = (0..)
                    .map_while(|i| tab.view.observe_controllers().item(i))
                    .filter_map(|c| c.downcast::<gtk::DropTarget>().ok())
                    .find(|t| t.types().contains(&glib::Type::STRING))
                    .expect("bench drop target");
                println!(
                    "bench style_drop types={:?} text={dropped:?}",
                    target.types()
                );
                let value = glib::BoxedValue(dropped.to_value());
                target.emit_by_name::<bool>("drop", &[&value, &0.0f64, &0.0f64]);
            }
            _ => {
                tab.view.emit_copy_clipboard();
                tab.buffer.place_cursor(&tab.buffer.iter_at_offset(into));
                tab.view.emit_paste_clipboard();
            }
        }
        // The clipboard is read asynchronously, even when it is this process that owns it.
        glib::timeout_future(Duration::from_millis(100)).await;
        let over = |name: &str| {
            tab.buffer.tag_table().lookup(name).is_some_and(|tag| {
                (into..into + len).any(|at| tab.buffer.iter_at_offset(at).has_tag(&tag))
            })
        };
        // The line it landed in, so a paste that brought nothing cannot pass for a clean one.
        let row = tab.buffer.iter_at_offset(into).line() as usize;
        let line = tab.text().lines().nth(row).unwrap_or_default().to_string();
        println!(
            "bench style_paste case={case} line={line:?} h1={} strong={} fold={}",
            over("h1"),
            over("strong"),
            over("fold")
        );
    }
}

/// Fill `tab` with `chars` of body, then type `# Heading` on a line of its own, one character at a
/// time the way a keyboard delivers it.
fn bench_style_typing(tab: &Rc<Tab>, chars: usize) {
    // Exactly 32 bytes, so the body is exactly the size the numbers are labelled with.
    let body = "filler text for a long-ish note\n";
    tab.set_text(&body.repeat(chars / body.len()));
    tab.buffer.place_cursor(&tab.buffer.end_iter());
    for ch in "\n# Headin".chars() {
        tab.buffer.insert_at_cursor(&ch.to_string());
    }
    let t0 = Instant::now();
    tab.buffer.insert_at_cursor("g");
    let us = t0.elapsed().as_micros();
    let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
    println!(
        "bench style_sync chars={chars} {}",
        bench_heading_at(tab, line)
    );
    println!("bench style_us {us}");
}

/// Type the same heading *inside a fenced block* on a note too long for a full pass. The line is
/// tagged from a parse of the whole document, so the fence above it is what decides what it is:
/// this is the claim a per-line pass stands or falls on, printed rather than argued.
fn bench_style_fenced(tab: &Rc<Tab>) {
    let body = "filler text for a long-ish note\n".repeat(1024);
    tab.set_text(&format!("{body}```\n\n```\n"));
    let Some(inside) = tab.buffer.iter_at_line(1025) else {
        return;
    };
    tab.buffer.place_cursor(&inside);
    for ch in "# Heading".chars() {
        tab.buffer.insert_at_cursor(&ch.to_string());
    }
    let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
    println!(
        "bench style_fenced h1={} codeblock={}",
        bench_tag_at(tab, line, "h1"),
        bench_tag_at(tab, line, "codeblock")
    );
}

fn bench_heading_at(tab: &Rc<Tab>, line: i32) -> bool {
    bench_tag_at(tab, line, "h1")
}

fn bench_tag_at(tab: &Rc<Tab>, line: i32, name: &str) -> bool {
    let Some(tag) = tab.buffer.tag_table().lookup(name) else {
        return false;
    };
    tab.buffer
        .iter_at_line(line)
        .is_some_and(|iter| iter.has_tag(&tag))
}

/// Select things in the note at `rel` and print what the muted occurrence highlight made of each:
/// the query it took and the character ranges it painted. Then open the find bar on the same word,
/// so the last lines say what happens where a find-bar match and a muted occurrence land on the
/// same text: both tags are on it, and the find bar's is the higher priority of the two.
///
/// The buffer is filled with text of its own first: the ranges are the point, and they have to be
/// the bench's rather than whatever the vault generator wrote. Nothing is saved — the run quits
/// well inside the one-second autosave.
/// What a Ctrl+hover underlines, and what following the same link does when nothing answers to
/// it. The underline's link half only: a plain word is a question for a language server, and the
/// vault a drill runs against holds notes rather than code.
pub(super) fn bench_follow(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        // ASCII throughout, so `find` gives the character offset the buffer counts in.
        let text = "See [[Other Note]] and a plain word here.\n";
        tab.set_text(text);
        // The link table is filled by the analysis debounce, not by the edit.
        glib::timeout_add_local_once(Duration::from_millis(400), move || {
            let at = |needle: &str| text.find(needle).expect("bench needle") as i32;
            let probe = |label: &str, needle: &str, ctrl: bool| {
                let where_ = tab
                    .view
                    .iter_location(&tab.buffer.iter_at_offset(at(needle)));
                let (x, y) = tab.view.buffer_to_window_coords(
                    gtk::TextWindowType::Widget,
                    where_.x() + 1,
                    where_.y() + 1,
                );
                tab.follow_hint(x as f64, y as f64, ctrl);
                println!("bench follow {label} underlined={:?}", tab.follow_shown());
            };
            // The link underlines whole, markers and all; the prose beside it does not.
            probe("wikilink", "Other", true);
            probe("plain_word", "plain", true);
            probe("wikilink_again", "Other", true);
            // Ctrl up over the same link takes it off again.
            probe("ctrl_released", "Other", false);
            glib::spawn_future_local(bench_dangling(app));
        });
    });
}

/// Follow a link nothing in the vault answers to: New File comes up with the path the link spells
/// already typed in. Both the resolve and the vault's templates arrive from a worker, so the
/// dialog is waited for rather than assumed. Cancelling it is [`bench_close`]'s drill.
async fn bench_dangling(app: Rc<App>) {
    app.open_target("Nowhere/Other Note");
    for _ in 0..40 {
        if app.window.visible_dialog().is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    let dialog = app
        .window
        .visible_dialog()
        .and_then(|d| d.downcast::<adw::AlertDialog>().ok());
    let typed = dialog
        .as_ref()
        .and_then(|d| d.extra_child())
        .and_then(|form| find_widget(&form, &|w| w.is::<gtk::Entry>()))
        .and_downcast::<gtk::Entry>()
        .map(|entry| entry.text());
    println!(
        "bench follow dangling heading={:?} typed={typed:?}",
        dialog.and_then(|d| d.heading())
    );
    bench_quit(&app);
}

pub(super) fn bench_occurrences(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        // ASCII throughout, so the byte offset `find` gives is also the character offset the
        // buffer counts in.
        let text = "Alpha beta alpha\ngamma ALPHA delta\nx y\n";
        tab.set_text(text);
        let (find_colour, muted_colour) = tab.match_colours();
        println!("bench occur colours find={find_colour:?} muted={muted_colour:?}");

        for (label, selected) in [
            // Two characters, and the three occurrences differ in case: the shortest selection
            // that highlights anything, matched the way the find bar matches.
            ("two_chars", "al"),
            ("word", "beta"),
            // A selection whose own case is the odd one out still finds the other two.
            ("cased", "ALPHA"),
            // Neither of these highlights anything.
            ("one_char", "x"),
            ("multi_line", "alpha\ngamma"),
        ] {
            let at = text.find(selected).expect("bench needle") as i32;
            tab.buffer.select_range(
                &tab.buffer.iter_at_offset(at),
                &tab.buffer
                    .iter_at_offset(at + selected.chars().count() as i32),
            );
            let (query, tag) = tab.occurrence_highlight();
            println!(
                "bench occur case={label} select={selected:?} query={query:?} at={:?}",
                bench_tag_ranges(&tab, &tag)
            );
        }

        // Both highlights on the same word. The find bar's tag has to be the higher priority of
        // the two, or the muted hint would paint over the match the user is stepping through —
        // and it has to stay that way across an edit, which is when gtksourceview re-raises its
        // own tag.
        tab.set_query("al");
        tab.set_highlight(true);
        bench_pump();
        let at = text.find("al").expect("bench needle") as i32;
        tab.buffer.select_range(
            &tab.buffer.iter_at_offset(at),
            &tab.buffer.iter_at_offset(at + 2),
        );
        let (_, muted) = tab.occurrence_highlight();
        let find = bench_search_tag(&tab, find_colour.as_deref());
        println!(
            "bench occur overlap find={:?} muted={:?}",
            find.as_ref()
                .map(|tag| bench_tag_ranges(&tab, tag))
                .unwrap_or_default(),
            bench_tag_ranges(&tab, &muted)
        );
        for what in ["unedited", "edited"] {
            println!(
                "bench occur priority {what} find={:?} muted={}",
                bench_search_tag(&tab, find_colour.as_deref()).map(|tag| tag.priority()),
                muted.priority()
            );
            tab.buffer.insert(&mut tab.buffer.end_iter(), "al\n");
            bench_pump();
        }
        bench_quit(&app);
    });
}

/// The tag the find bar's search context paints with, found by its colour: gtksourceview keeps
/// that tag to itself, and the scheme's `search-match` background is what it was given.
fn bench_search_tag(tab: &Rc<Tab>, colour: Option<&str>) -> Option<gtk::TextTag> {
    let wanted = gdk::RGBA::parse(colour?).ok()?;
    let mut found = None;
    tab.buffer.tag_table().foreach(|tag| {
        if found.is_none() && tag.is_background_set() && tag.background_rgba() == Some(wanted) {
            found = Some(tag.clone());
        }
    });
    found
}

/// Where `tag` is on, as character offsets.
fn bench_tag_ranges(tab: &Rc<Tab>, tag: &gtk::TextTag) -> Vec<(i32, i32)> {
    let mut ranges = Vec::new();
    let mut iter = tab.buffer.start_iter();
    loop {
        if !iter.starts_tag(Some(tag)) && !iter.forward_to_tag_toggle(Some(tag)) {
            return ranges;
        }
        let start = iter.offset();
        if !iter.forward_to_tag_toggle(Some(tag)) {
            ranges.push((start, tab.buffer.end_iter().offset()));
            return ranges;
        }
        ranges.push((start, iter.offset()));
    }
}
