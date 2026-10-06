//! Drills over a note's text: styling as it is typed, pastes, Ctrl+hover and occurrences.

use super::*;
use crate::editor::line_end;
use accent_core::config::Theme;
use sourceview5::prelude::BufferExt as _;

/// Type a heading into the note at `rel`, at a size that styles on the keystroke and at one that
/// used to wait for the debounce, and print whether the `h1` tag is on the line *before the main
/// loop turns again*. `changed` is emitted from inside the insert, so a `true` here can only have
/// come from the synchronous path — which is the whole question this bench answers.
pub(super) fn bench_style(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
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
                bench_style_retag(&tab).await;
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
        // the drag is ours and carries the hidden text too, which a move then takes away with the
        // rest of the range.
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
        // The text going gave up the primary selection the last case left, and on X11 the server
        // answers with a SelectionClear that GDK reads later. Read after the selection below has
        // claimed it again in the same millisecond, it passes for another owner's, and GTK
        // unselects the buffer before the middle click's read: nothing pasted. So it is read
        // first: the server answers before the sync returns, and GDK's events go ahead of an idle.
        tab.view.display().sync();
        glib::timeout_future_with_priority(glib::Priority::DEFAULT_IDLE, Duration::ZERO).await;
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
            // What the drag carries is ours across a fold (`editor::drag_content`) and the
            // selection's own content provider otherwise; the view's drop target reads it as a
            // string and inserts that where its `gtk_drag_target` mark is.
            "drop" => {
                let stream = gio::MemoryOutputStream::new_resizable();
                let content = crate::editor::drag_content(tab.buffer.upcast_ref())
                    .unwrap_or_else(|| tab.buffer.selection_content());
                let source = content
                    .value(content.formats().types()[0])
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

/// Edit a note the way a reader does — inside a heading and around one, a fence opened and closed
/// again, a list, a paste, an undo, beside a folded section — once at a size that restyles on the
/// keystroke and once at one that waits for the debounce, and print the tags that then lie
/// anywhere other than where a fresh pass over the same text puts them. `mismatch=0` is the claim
/// that re-tagging only what changed leaves what re-tagging everything would.
async fn bench_style_retag(tab: &Rc<Tab>) {
    const NOTE: &str = "# Title\n\nSome prose with **bold** and a [[Link]].\n\n## Fold me\n\
                        hidden line\n\n- one\n- two\n\nLast paragraph.\n";
    let settle = || glib::timeout_future(Duration::from_millis(300));
    for filler in [0, 20 * 1024] {
        let body = "filler text for a long-ish note\n".repeat(filler / 32);
        tab.set_text(&format!("{NOTE}{body}"));
        let fold = accent_api::Fold {
            start_line: 4,
            end_line: 5,
        };
        crate::fold::fold(tab.buffer.upcast_ref(), fold);
        // ASCII throughout, so a byte offset is also the character offset the buffer counts in.
        let at = |needle: &str| tab.text().find(needle).expect("bench needle") as i32;
        let typed = |offset: i32, text: &str| {
            tab.buffer.place_cursor(&tab.buffer.iter_at_offset(offset));
            for ch in text.chars() {
                tab.buffer.insert_at_cursor(&ch.to_string());
            }
        };
        let delete = |offset: i32, chars: i32| {
            let mut from = tab.buffer.iter_at_offset(offset);
            tab.buffer
                .delete(&mut from, &mut tab.buffer.iter_at_offset(offset + chars));
        };
        typed(at("Title"), "Big ");
        typed(at("Some prose"), "# ");
        settle().await;
        delete(at("# Some prose"), 2);
        typed(at("- one"), "```\n");
        settle().await;
        typed(at("Last"), "```\n");
        settle().await;
        delete(at("```\n- one"), 4);
        typed(at("Last"), "- three\n");
        settle().await;
        let mut end = tab.buffer.iter_at_offset(at("Last"));
        tab.buffer
            .insert(&mut end, "## Pasted\n\n*em* and `code`\n");
        settle().await;
        let undone = tab.buffer.can_undo();
        tab.buffer.undo();
        settle().await;
        let off = crate::highlight::mismatches(&tab.buffer);
        println!(
            "bench style_retag chars={} undone={undone} mismatch={} {off:?}",
            tab.buffer.char_count(),
            off.len()
        );
    }
}

/// Typed words, a character every [`TYPING_EVERY`], [`TYPING_KEYS`] of them per size.
const TYPING_WORDS: &str = "the quick brown fox jumps over the lazy dog ";
const TYPING_KEYS: usize = 40;
const TYPING_EVERY: Duration = Duration::from_millis(150);
/// What a note is made of at every size: a section of prose with the markup a note carries.
const TYPING_SECTION: &str = "## Section\n\nSome prose with **bold**, *emphasis*, `code`, a \
    [[Wiki Link]] and a #tag, plus a [link](https://example.org). A second sentence that runs on \
    for a while, so that the line wraps in a window of ordinary width, as prose does.\n\n\
    - a list item with **bold**\n- [ ] a task\n- [x] a done task\n\n> a quote with *emphasis*\n\n\
    ```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n";

/// `ACCENT_BENCH_STYLE=typing:<rel>` fills the note at `rel` with 4, 15, 64 and 256 KB of
/// sections and types into the middle of each at a key every 150 ms, printing the main thread's
/// CPU time over the run against the wall time: what a keystroke costs once GTK has laid out
/// whatever the restyle touched, which a timer around the restyle itself cannot see. 15 rather
/// than 16 so that size styles on every keystroke, below `editor::INSTANT`. `idle` is the same
/// share over the second before the typing, so a layout of the fill still running shows,
/// `key_us` the median keystroke's own time, the styling it runs before it returns included, and
/// `pass_us` what one more full pass over the typed note costs, changing nothing.
/// `typing:<rel>:<kb>` types into that one size, for a profiler.
pub(super) fn bench_typing(app: &Rc<App>, arg: &str) {
    let (rel, sizes) = match arg.rsplit_once(':') {
        Some((rel, kb)) if kb.parse::<usize>().is_ok() => (rel, vec![kb.parse().unwrap_or(4)]),
        _ => (arg, vec![4, 15, 64, 256]),
    };
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        // CPU time of the thread asking, the main one here, in nanoseconds.
        let cpu = || {
            std::fs::read_to_string("/proc/thread-self/schedstat")
                .ok()
                .and_then(|s| s.split_whitespace().next()?.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let busy = |cpu: u64, wall: Duration| cpu as f64 / wall.as_nanos() as f64;
        for kb in sizes {
            tab.set_text(&TYPING_SECTION.repeat(kb * 1024 / TYPING_SECTION.len()));
            let middle = tab.buffer.line_count() / 2;
            if let Some(at) = tab.buffer.iter_at_line(middle) {
                tab.buffer.place_cursor(&at);
            }
            tab.view
                .scroll_to_mark(&tab.buffer.get_insert(), 0.0, true, 0.0, 0.5);
            glib::timeout_future(Duration::from_secs(3)).await;
            let (idle, t0) = (cpu(), Instant::now());
            glib::timeout_future(Duration::from_secs(1)).await;
            let idle = busy(cpu() - idle, t0.elapsed());
            let (cpu0, t0) = (cpu(), Instant::now());
            let mut keys = Vec::with_capacity(TYPING_KEYS);
            for ch in TYPING_WORDS.chars().cycle().take(TYPING_KEYS) {
                let key = Instant::now();
                tab.buffer.insert_at_cursor(&ch.to_string());
                keys.push(key.elapsed().as_micros());
                glib::timeout_future(TYPING_EVERY).await;
            }
            keys.sort_unstable();
            // The debounced pass after the last key, and the layout it leaves.
            glib::timeout_future(Duration::from_millis(500)).await;
            let (used, wall) = (cpu() - cpu0, t0.elapsed());
            // One full pass over the note as it now stands, which changes nothing.
            let pass = Instant::now();
            crate::highlight::apply(&tab.buffer);
            println!(
                "bench style_typing kb={kb} chars={} keys={TYPING_KEYS} wall_ms={} cpu_ms={} \
                 busy={:.2} idle={idle:.2} key_us={} pass_us={}",
                tab.buffer.char_count(),
                wall.as_millis(),
                used / 1_000_000,
                busy(used, wall),
                keys[TYPING_KEYS / 2],
                pass.elapsed().as_micros()
            );
        }
        bench_quit(&app);
    });
}

/// The pointer's half of `drop_fold`, held for XTEST: a section folded under its heading and
/// selected whole, where to press on it and where to let it go at the end of the note, and five
/// seconds later what the note holds and the action a drag of ours ended with, ten times, every
/// other one meant to be driven with Ctrl held. A move has to carry the hidden body and take all
/// of it away from where it was, so `hidden body` is in the note once, after `plain line`; a copy
/// leaves it twice. Each round says whether the drag was ours (`ended=Some`), and `lost=true`
/// where the hidden body left the note; the last line counts the rounds whose drag was GTK's own
/// (`gtk`, the note changed with no drag of ours ending), that changed nothing (`missed`) and that
/// lost text (`lost`), the invariant being that a drag never takes away text it does not carry.
/// Drive it in steps with pauses between them — `move X0 Y0; down`, a move past the drag
/// threshold, then `sleep 0.3; move X1 Y1; sleep 0.5; up` — because XDND's position and status
/// messages have to go round before the release, and `xtest.py`'s one-shot `drag` lets go too
/// soon.
pub(super) fn bench_drag_fold(app: &Rc<App>, rel: &str) {
    const FOLDED: &str = "# One\nhidden body\n# Two\nplain line\n";
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(800)).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let at = |needle: &str| {
            let offset = FOLDED.find(needle).expect("bench needle") as i32;
            tab.buffer.iter_at_offset(offset)
        };
        // Screen coordinates: under Xvfb with no window manager the window sits at 0,0.
        let screen = |iter: &gtk::TextIter| {
            let r = tab.view.iter_location(iter);
            let (x, y) = tab.view.buffer_to_window_coords(
                gtk::TextWindowType::Widget,
                r.x() + 2,
                r.y() + r.height() / 2,
            );
            let point = gtk::graphene::Point::new(x as f32, y as f32);
            let p = tab.view.compute_point(&app.window, &point).unwrap_or(point);
            let (sx, sy) = app.window.surface_transform();
            ((p.x() as f64 + sx) as i32, (p.y() as f64 + sy) as i32)
        };
        let (mut gtk, mut missed, mut lost) = (0, 0, 0);
        for round in 0..10 {
            let kind = ["plain", "ctrl"][round % 2];
            tab.set_text(FOLDED);
            let fold = accent_api::Fold {
                start_line: 0,
                end_line: 1,
            };
            crate::fold::fold(tab.buffer.upcast_ref(), fold);
            tab.buffer.select_range(&at("# One"), &at("# Two"));
            tab.view.grab_focus();
            glib::timeout_future(Duration::from_millis(300)).await;
            let mut end = tab.buffer.end_iter();
            end.backward_char();
            crate::editor::DRAG_ENDED.set(None);
            println!(
                "bench drag_fold {round} {kind} press={:?} release={:?}",
                screen(&at("One")),
                screen(&end)
            );
            glib::timeout_future(Duration::from_secs(5)).await;
            let (ended, text) = (crate::editor::DRAG_ENDED.get(), tab.text());
            let gone = !text.contains("hidden body");
            gtk += usize::from(ended.is_none() && text != FOLDED);
            missed += usize::from(ended.is_none() && text == FOLDED);
            lost += usize::from(gone);
            println!("bench drag_fold {round} {kind} ended={ended:?} lost={gone} text={text:?}");
        }
        println!("bench drag_fold summary rounds=10 gtk={gtk} missed={missed} lost={lost}");
        bench_quit(&app);
    });
}

/// Delete at the end of a folded heading and Backspace at the start of the line after its fold,
/// each of which joins a visible line to a hidden one, then ask for the iter at every pixel row of
/// the note, as GtkSourceView asks at the top and bottom of the screen on every frame. A line left
/// partly hidden aborts the process there (GTK's "Byte index … is off the end of the line"); kept
/// to whole lines, each case prints what is hidden and what the joined line reads. Then `stale`
/// shuts the fold and runs a Ctrl-held pointer down every row before GTK has measured the hidden
/// lines again, and `screen` draws the view with its top row below a partly hidden line, where
/// GtkSourceView's gutter and annotations ask GTK for the iter: both aborted the same way.
pub(super) fn bench_seam(app: &Rc<App>, rel: &str) {
    // Short lines after the fold: GTK's walk past the joined line lands in a line too short for
    // the bytes it carried along, which is what aborts.
    const FOLDED: &str = "# One\na hidden line\nhidden\nbody\n# Two\nplain line\n";
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(800)).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        for case in ["delete", "backspace"] {
            tab.set_text(FOLDED);
            let fold = accent_api::Fold {
                start_line: 0,
                end_line: 3,
            };
            crate::fold::fold(tab.buffer.upcast_ref(), fold);
            // The keys themselves, from where each one stands: GTK deletes the character next to
            // the caret whether it is hidden or not.
            match case {
                "delete" => {
                    tab.buffer.place_cursor(&line_end(&tab.buffer, 0));
                    tab.view.emit_delete_from_cursor(gtk::DeleteType::Chars, 1);
                }
                _ => {
                    let at = FOLDED.find("# Two").expect("bench needle") as i32;
                    tab.buffer.place_cursor(&tab.buffer.iter_at_offset(at));
                    tab.view.emit_backspace();
                }
            }
            // A frame, so the lines are measured as they are drawn.
            glib::timeout_future(Duration::from_millis(200)).await;
            let (y, height) = tab.view.line_yrange(&tab.buffer.end_iter());
            for y in 0..y + height {
                tab.view.iter_at_location(0, y);
            }
            let hidden: String = tab
                .text()
                .chars()
                .enumerate()
                .filter(|(i, _)| {
                    !crate::fold::hiding(
                        tab.buffer.upcast_ref(),
                        &tab.buffer.iter_at_offset(*i as i32),
                    )
                    .is_empty()
                })
                .map(|(_, c)| c)
                .collect();
            let caret = crate::editor::caret(&tab.buffer).line();
            let line = tab
                .text()
                .lines()
                .nth(caret as usize)
                .unwrap_or_default()
                .to_string();
            println!("bench seam case={case} hidden={hidden:?} line={line:?}");
        }
        // A fold shut in the frame a Ctrl-held pointer moves in: its lines keep the height they
        // were drawn at until GTK measures them again, so the pointer's rows fall on hidden lines,
        // and `Tab::follow_hint` asked GTK for the iter there (`fold::iter_at_location`).
        tab.set_text(FOLDED);
        glib::timeout_future(Duration::from_millis(200)).await;
        let (top, height) = tab.view.line_yrange(&tab.buffer.end_iter());
        let fold = accent_api::Fold {
            start_line: 0,
            end_line: 3,
        };
        crate::fold::fold(tab.buffer.upcast_ref(), fold);
        for y in 0..top + height {
            let (x, y) = tab
                .view
                .buffer_to_window_coords(gtk::TextWindowType::Widget, 0, y);
            tab.follow_hint(f64::from(x), f64::from(y), true);
        }
        tab.follow_hint(0.0, 0.0, false);
        println!(
            "bench seam case=stale folded={}",
            crate::fold::is_folded(tab.buffer.upcast_ref(), 0)
        );
        // The screen's top row in the pixels below a partly hidden line, as the view is drawn:
        // GtkSourceView's annotations ask GTK for the iter there on every frame. The tag goes on
        // without an edit, which is all `whole_lines` answers to.
        let body: String = (0..200).map(|i| format!("body line {i}\n")).collect();
        tab.set_text(&format!("# One\n{body}# Two\nplain line\n"));
        glib::timeout_future(Duration::from_millis(200)).await;
        let line = tab.buffer.iter_at_line(50).expect("bench line");
        let (top, height) = tab.view.line_yrange(&line);
        if let Some(adjustment) = tab.view.vadjustment() {
            adjustment.set_value(f64::from(top + height - 1 + tab.view.top_margin()));
        }
        glib::timeout_future(Duration::from_millis(200)).await;
        if let Some(tag) = tab.buffer.tag_table().lookup(crate::fold::TAG) {
            let mut from = line;
            from.forward_chars(5);
            let to = tab.buffer.iter_at_line(61).expect("bench line");
            tab.buffer.apply_tag(&tag, &from, &to);
        }
        tab.view.queue_draw();
        if let Some(parent) = tab.view.parent() {
            parent.snapshot_child(&tab.view, &gtk::Snapshot::new());
        }
        println!(
            "bench seam case=screen top_in_line={}",
            tab.view.visible_rect().y() - top
        );
        // The layout's height wrapped below zero, as a comparison's runaway padding took it, with
        // the first lines hidden as a comparison's collapsed run hides them: GTK clamps every row
        // it is asked about into that height, which finds hidden line 0 whatever the row, and the
        // ask at the screen's bottom row walked on from there.
        let body: String = (0..60).map(|i| format!("body line {i}\n")).collect();
        tab.set_text(&format!("# One\n{body}"));
        if let Some(tag) = tab.buffer.tag_table().lookup(crate::fold::TAG) {
            let to = tab.buffer.iter_at_line(5).expect("bench line");
            tab.buffer.apply_tag(&tag, &tab.buffer.start_iter(), &to);
        }
        let huge = gtk::TextTag::builder()
            .pixels_above_lines(i32::MAX / 2)
            .pixels_below_lines(i32::MAX / 2)
            .build();
        tab.buffer.tag_table().add(&huge);
        let line = tab.buffer.iter_at_line(30).expect("bench line");
        let mut end = line;
        end.forward_char();
        tab.buffer.apply_tag(&huge, &line, &end);
        if let Some(adjustment) = tab.view.vadjustment() {
            adjustment.set_value(0.0);
        }
        glib::timeout_future(Duration::from_millis(200)).await;
        if let Some(parent) = tab.view.parent() {
            parent.snapshot_child(&tab.view, &gtk::Snapshot::new());
        }
        let (y, height) = tab.view.line_yrange(&tab.buffer.end_iter());
        println!(
            "bench seam case=overflow layout_height={} visible_y={}",
            y + height,
            tab.view.visible_rect().y()
        );
        bench_quit(&app);
    });
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

/// Open each of `rels` in turn in a narrow window and print, for every line, the wrap tag on its
/// first character, that tag's indent, and how far right of the line's first screen row its second
/// one starts: the hang as GTK laid it out, `None` for a line that does not wrap. Each tab stays up
/// for four seconds, for a screenshot; a note is first given a fence opened above everything, and
/// closed again, with every line's tag printed each time. The last one is then filled with 10k
/// indented lines, three to a depth as code's blocks come — in a note, list and quote items behind
/// a bullet, a number or a `>` in turn — and the fill, a keystroke, a Return, a restyle's measure
/// and a new indent width are timed, each with the tag it left on the line it touched.
pub(super) fn bench_wrap(app: &Rc<App>, rels: &str) {
    let app = app.clone();
    let rels: Vec<String> = rels.split(',').map(str::to_string).collect();
    app.window.set_default_size(900, 880);
    glib::spawn_future_local(async move {
        let mut last = None;
        for rel in &rels {
            app.open_path(rel);
            glib::timeout_future(Duration::from_millis(1500)).await;
            let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == *rel) else {
                println!("bench wrap {rel} not_open");
                continue;
            };
            for line in 0..tab.buffer.line_count() {
                println!("bench wrap {rel} {}", bench_wrap_line(&tab, line));
            }
            // A fence typed open above a note takes in every line below it, which then wraps as
            // code; taken out again, they hang behind their markers once more.
            if tab.flavour().is_note() {
                let tags = |tab: &Rc<Tab>| {
                    (0..tab.buffer.line_count())
                        .map(|line| bench_wrap_tag(tab, line))
                        .collect::<Vec<_>>()
                };
                tab.buffer.insert(&mut tab.buffer.start_iter(), "```\n");
                println!("bench wrap_fence_opened {rel} {:?}", tags(&tab));
                if let Some(mut second) = tab.buffer.iter_at_line(1) {
                    tab.buffer.delete(&mut tab.buffer.start_iter(), &mut second);
                }
                println!("bench wrap_fence_closed {rel} {:?}", tags(&tab));
            }
            println!("bench wrap_hold {rel}");
            glib::timeout_future(Duration::from_secs(4)).await;
            last = Some(tab);
        }
        if let Some(tab) = last {
            bench_wrap_cost(&tab);
        }
        bench_quit(&app);
    });
}

fn bench_wrap_line(tab: &Rc<Tab>, line: i32) -> String {
    let Some(start) = tab.buffer.iter_at_line(line) else {
        return format!("line={line} missing");
    };
    let tag = start
        .tags()
        .into_iter()
        .find(|tag| tag.name().is_some_and(|name| name.starts_with("wrap")));
    let mut row = start;
    let hang = (tab.view.forward_display_line(&mut row) && !row.starts_line())
        .then(|| tab.view.iter_location(&row).x() - tab.view.iter_location(&start).x());
    let mut head = start;
    head.forward_chars(12);
    // On a note's list or quote line, how far right its text starts behind the marker on the first
    // row, which is where the hang should put the rows after it.
    let text = tab.buffer.text(&start, &line_end(&tab.buffer, line), true);
    let (_, marker) = crate::typing::wrap_head(&text, 8, tab.flavour().is_note());
    let text_x = (!marker.is_empty()).then(|| {
        let indent = text.len() - text.trim_start_matches([' ', '\t']).len();
        let mut at = start;
        at.forward_chars((indent + marker.len()) as i32);
        tab.view.iter_location(&at).x() - tab.view.iter_location(&start).x()
    });
    format!(
        "line={line} head={:?} tag={:?} indent={:?} hang={hang:?} text_x={text_x:?}",
        tab.buffer
            .text(&start, &head.min(line_end(&tab.buffer, line)), true),
        tag.as_ref().and_then(|tag| tag.name()),
        tag.map(|tag| tag.indent()),
    )
}

/// The wrap tag on line `line`'s first character, by name.
fn bench_wrap_tag(tab: &Rc<Tab>, line: i32) -> Option<String> {
    tab.buffer
        .iter_at_line(line)?
        .tags()
        .into_iter()
        .find_map(|tag| {
            tag.name()
                .filter(|name| name.starts_with("wrap"))
                .map(|name| name.to_string())
        })
}

fn bench_wrap_cost(tab: &Rc<Tab>) {
    let lines = 10_000;
    let note = tab.flavour().is_note();
    let body: String = (0..lines)
        .map(|i| match note {
            true => format!(
                "{}{} item {i} of a list\n",
                "  ".repeat(i / 3 % 6),
                ["-", "1.", ">"][i % 3]
            ),
            false => format!(
                "{}let value_{i} = compute({i});\n",
                "    ".repeat(1 + i / 3 % 6)
            ),
        })
        .collect();
    let t0 = Instant::now();
    tab.buffer.set_text(&body);
    let fill = ms_since(t0);
    // The wrap pass alone, over every line with its tags already on.
    let t0 = Instant::now();
    tab.view.notify("tab-width");
    let recheck = ms_since(t0);
    println!(
        "bench wrap_cost lines={lines} fill_ms={fill:.1} recheck_ms={recheck:.1} line1={:?}",
        bench_wrap_tag(tab, 1)
    );
    // What every restyle asks of the wrap tags with the font unchanged.
    let t0 = Instant::now();
    crate::wrap::measure(&tab.view);
    println!("bench wrap_measure us={}", t0.elapsed().as_micros());
    let line = 5000;
    tab.buffer.place_cursor(&line_end(&tab.buffer, line));
    let t0 = Instant::now();
    tab.buffer.insert_at_cursor("x");
    println!(
        "bench wrap_key us={} tag={:?}",
        t0.elapsed().as_micros(),
        bench_wrap_tag(tab, line)
    );
    // Return and the auto-indent behind it, as one insert.
    let t0 = Instant::now();
    tab.buffer.insert_at_cursor("\n            ");
    println!(
        "bench wrap_return us={} tag={:?}",
        t0.elapsed().as_micros(),
        bench_wrap_tag(tab, line + 1)
    );
    // The same line's indent taken away again, and then half of the line above's.
    let mut start = tab.buffer.iter_at_line(line + 1).expect("bench line");
    let mut indent = start;
    indent.forward_chars(12);
    tab.buffer.delete(&mut start, &mut indent);
    let mut start = tab.buffer.iter_at_line(line).expect("bench line");
    let mut half = start;
    half.forward_chars(2);
    tab.buffer.delete(&mut start, &mut half);
    println!(
        "bench wrap_unindent tag={:?} above={:?}",
        bench_wrap_tag(tab, line + 1),
        bench_wrap_tag(tab, line)
    );
    // A hundred plain lines pasted into the middle of an indented one, where they arrive inside
    // its tag: the line keeps it, none of the pasted ones may, and the rest of the line, now a line
    // of its own behind a space, hangs a level past that space.
    let at = line + 10;
    let mut middle = tab.buffer.iter_at_line(at).expect("bench line");
    middle.forward_chars(24);
    let t0 = Instant::now();
    tab.buffer.insert(&mut middle, &"pasted\n".repeat(100));
    let us = t0.elapsed().as_micros();
    let pasted: Vec<_> = (at + 1..at + 100)
        .filter_map(|n| bench_wrap_tag(tab, n))
        .collect();
    println!(
        "bench wrap_paste us={us} line={:?} tagged_pasted={pasted:?} rest={:?}",
        bench_wrap_tag(tab, at),
        bench_wrap_tag(tab, at + 100)
    );
    // Four columns a level becomes two, which moves every indented line.
    let t0 = Instant::now();
    tab.set_indent_width(2);
    println!(
        "bench wrap_indent_width ms={:.1} line1={:?}",
        ms_since(t0),
        bench_wrap_tag(tab, 1)
    );
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
/// vault a drill runs against holds notes rather than code. A bare URL underlines in any text
/// file, so pointed at a `.txt` the drill still underlines it, and only it.
pub(super) fn bench_follow(app: &Rc<App>, rel: &str) {
    if let Some(rel) = rel.strip_prefix("hover:") {
        return bench_hover(app, rel);
    }
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        // ASCII throughout, so `find` gives the character offset the buffer counts in.
        let text = "See [[Other Note]] and a plain word here, https://e.org/a.\n\
                    [[Nowhere/Other Note#Part|there]]\n";
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
            // A bare URL is a link in any text, and what the caret is on is what F12 would open.
            probe("url", "e.org", true);
            tab.buffer
                .place_cursor(&tab.buffer.iter_at_offset(at("e.org")));
            println!("bench follow url_at_caret={:?}", tab.url_at_cursor());
            // Ctrl up over the same link takes it off again.
            probe("ctrl_released", "Other", false);
            let caret = tab.buffer.iter_at_offset(at("Nowhere"));
            tab.buffer.place_cursor(&caret);
            glib::spawn_future_local(bench_dangling(app, tab));
        });
    });
}

/// The hover over the first wikilink of the note at `rel`, zoomed in twice, through the real
/// pointer: prints `bench hover_aim <x> <y>` for `build-aux/xtest.py :N "move <x> <y>"`, then, once
/// the hover is up, its font beside the note's, its size beside the window's, a line of it beside
/// one of the note's and how tall its whole text is, and `emptied`: the hover emptied while up, as
/// GtkSourceView empties it before asking again, which aborted. Then `bench hover_scroll_aim <x>
/// <y>`, the middle of the hover, for the pointer to be walked into (a jump there dismisses it)
/// and a wheel turned, and what the hover's scroll came to. Held on screen 5 s in all, long enough
/// for `import -window root -display :N shot.png`.
fn bench_hover(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1000)).await;
        let Some(tab) = app.active() else {
            return bench_quit(&app);
        };
        // Zoomed, as a reader who finds the text small has it.
        for _ in 0..2 {
            let _ = WidgetExt::activate_action(&app.window, "win.zoom-in", None);
        }
        glib::timeout_future(Duration::from_millis(500)).await;
        let text = tab.text();
        let link = text
            .match_indices("[[")
            .map(|(at, _)| at)
            .find(|&at| !text[..at].ends_with('!'));
        let Some(link) = link else {
            println!("bench hover no_link");
            return bench_quit(&app);
        };
        let iter = tab
            .buffer
            .iter_at_offset(text[..link + 3].chars().count() as i32);
        tab.view
            .scroll_to_iter(&mut iter.clone(), 0.0, true, 0.5, 0.5);
        glib::timeout_future(Duration::from_millis(800)).await;
        let rect = tab.view.iter_location(&iter);
        let (x, y) = tab.view.buffer_to_window_coords(
            gtk::TextWindowType::Widget,
            rect.x() + 4,
            rect.y() + rect.height() / 2,
        );
        let point = graphene::Point::new(x as f32, y as f32);
        let point = tab.view.compute_point(&app.window, &point).unwrap_or(point);
        println!("bench hover_aim {:.0} {:.0}", point.x(), point.y());
        let mut popover = None;
        for _ in 0..50 {
            glib::timeout_future(Duration::from_millis(100)).await;
            popover = find_widget(tab.view.upcast_ref(), &|w| {
                w.is_mapped() && w.type_().name() == "GtkSourceHoverAssistant"
            });
            if popover.is_some() {
                break;
            }
        }
        let Some(popover) = popover else {
            println!("bench hover none");
            return bench_quit(&app);
        };
        glib::timeout_future(Duration::from_millis(300)).await;
        let label = find_widget(&popover, &|w| w.is::<gtk::Label>()).and_downcast::<gtk::Label>();
        let font = |w: &gtk::Widget| w.pango_context().font_description().map(|d| d.to_string());
        println!(
            "bench hover fonts label={:?} view={:?}",
            label.as_ref().and_then(|l| font(l.upcast_ref())),
            font(tab.view.upcast_ref())
        );
        let line = label.as_ref().map_or(0, |label| {
            let layout = label.layout();
            layout.pixel_size().1 / layout.line_count().max(1)
        });
        println!(
            "bench hover size={}x{} window={}x{} line_px={line} note_line_px={} text_px={}",
            popover.width(),
            popover.height(),
            app.window.width(),
            app.window.height(),
            rect.height(),
            label.as_ref().map_or(0, |label| label.height()),
        );
        // GtkSourceView empties a hover that is up before asking the providers again, when the
        // pointer settles on other text (`_gtk_source_hover_assistant_display`), and the view's
        // allocations meanwhile present it as it is: emptied, it measured 0 wide, which
        // `gdk_popup_present` refuses with a critical.
        let display = find_widget(&popover, &|w| w.is::<sourceview5::HoverDisplay>())
            .and_downcast::<sourceview5::HoverDisplay>();
        let content = find_widget(&popover, &|w| w.is::<gtk::ScrolledWindow>());
        if let (Some(display), Some(content)) = (display, content) {
            display.remove(&content);
            tab.view.queue_allocate();
            glib::timeout_future(Duration::from_millis(300)).await;
            println!("bench hover emptied up={}", popover.is_mapped());
            display.append(&content);
            glib::timeout_future(Duration::from_millis(300)).await;
        }
        // A wheel over the hover scrolls what did not fit, and the hover stays up. The popup's
        // surface is placed against the window's, which under Xvfb sits at the screen's origin.
        let scroller = find_widget(&popover, &|w| w.is::<gtk::ScrolledWindow>())
            .and_downcast::<gtk::ScrolledWindow>();
        let popup = popover
            .native()
            .and_then(|n| n.surface())
            .and_downcast::<gdk::Popup>();
        if let Some(popup) = popup {
            let surface = popup.upcast_ref::<gdk::Surface>();
            println!(
                "bench hover_scroll_aim {} {}",
                popup.position_x() + surface.width() / 2,
                popup.position_y() + surface.height() / 2
            );
            glib::timeout_future(Duration::from_millis(2000)).await;
            println!(
                "bench hover scrolled={:?} up={}",
                scroller.map(|s| s.vadjustment().value()),
                popover.is_mapped()
            );
        }
        glib::timeout_future(Duration::from_secs(3)).await;
        bench_quit(&app);
    });
}

/// Follow the link under the caret, which nothing in the vault answers to, the way F12, the
/// chord and a Ctrl+click do: New File comes up with the path the link spells already typed in,
/// its anchor and alias left out.
///
/// Then `[[#Nowhere]]`, a heading the note does not have: `bad_anchor` prints the caret's line,
/// which must be the top of the note, and the toast saying why; `preview_bad_anchor` the same for
/// a click on `[[#Elsewhere]]` in the preview, which printed the caret's own line and no toast
/// before the two shared a landing (`App::no_heading`). Then `block` and `preview_block`, the
/// same for a `^id` the note has and one it lacks. Last `late`: a link typed at the end of a note
/// past 16 K characters and followed at once, before the pause its analysis waits for, which must
/// offer New File as the first did — it printed `typed=None` while the server was sent the text
/// only after that pause.
async fn bench_dangling(app: Rc<App>, tab: Rc<Tab>) {
    app.go_to_definition();
    bench_new_file(&app, "dangling").await;
    // Past the close animation, so the last case waits for a dialog of its own.
    glib::timeout_future(Duration::from_millis(500)).await;

    tab.set_text("# Intro\ntext\n[[#Nowhere]]\n");
    tab.buffer.place_cursor(&tab.buffer.iter_at_offset(18));
    app.go_to_definition();
    glib::timeout_future(Duration::from_millis(300)).await;
    let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
    let said = super::compare::bench_toast(&app);
    println!("bench follow bad_anchor caret_line={line} said={said:?}");

    // What the preview hands over for a click on `[[#Elsewhere]]`, with the first toast taken
    // away so the one read is this case's own.
    app.toasts.dismiss_all();
    glib::timeout_future(Duration::from_millis(500)).await;
    tab.buffer.place_cursor(&tab.buffer.iter_at_offset(18));
    app.open_target("#Elsewhere");
    glib::timeout_future(Duration::from_millis(300)).await;
    let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
    let said = super::compare::bench_toast(&app);
    println!("bench follow preview_bad_anchor caret_line={line} said={said:?}");

    // A block id, followed from the editor and then as the preview hands it over: both put the
    // caret on the paragraph the id ends, line 1, saying nothing; one the note lacks goes to
    // the top and says it is a block. Each case with the toasts before it taken away.
    let quiet = || async {
        app.toasts.dismiss_all();
        glib::timeout_future(Duration::from_millis(500)).await;
    };
    let link = 30;
    quiet().await;
    tab.set_text("# Intro\nfirst\nsecond ^blk\n\n[[#^blk]]\n");
    tab.buffer.place_cursor(&tab.buffer.iter_at_offset(link));
    app.go_to_definition();
    glib::timeout_future(Duration::from_millis(300)).await;
    let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
    let said = super::compare::bench_toast(&app);
    println!("bench follow block caret_line={line} said={said:?}");
    for anchor in ["#^blk", "#^gone"] {
        quiet().await;
        tab.buffer.place_cursor(&tab.buffer.iter_at_offset(link));
        app.open_target(anchor);
        glib::timeout_future(Duration::from_millis(300)).await;
        let line = tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line();
        let said = super::compare::bench_toast(&app);
        println!("bench follow preview_block {anchor} caret_line={line} said={said:?}");
    }

    tab.set_text(&"word ".repeat(3400));
    // Past the server's refresh, so it holds the long text before the link is typed.
    glib::timeout_future(Duration::from_millis(600)).await;
    tab.buffer
        .insert(&mut tab.buffer.end_iter(), "[[Nowhere/Late]]");
    tab.buffer
        .place_cursor(&tab.buffer.iter_at_offset(tab.buffer.char_count() - 4));
    app.go_to_definition();
    // The typed link is the drill's, not the note's: it stops counting as an edit before the
    // dialog takes the focus from the view, which is a save.
    tab.discard();
    bench_new_file(&app, "late").await;
    bench_quit(&app);
}

/// Wait for the New File a followed link offers and print its heading and the name it arrives
/// with, then close it. Both the resolve and the vault's templates arrive from a worker, so the
/// dialog is waited for rather than assumed. What cancelling it leaves is [`bench_close`]'s drill.
async fn bench_new_file(app: &Rc<App>, case: &str) {
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
        "bench follow {case} heading={:?} typed={typed:?}",
        dialog.as_ref().and_then(|d| d.heading())
    );
    if let Some(dialog) = dialog {
        dialog.close();
    }
}

/// Walk the window through the themes and print what a note's own tags are painted in on each
/// side of every switch. `theme::apply` moves `AdwStyleManager`'s dark state, which is the very
/// signal a system switch raises, so the `notify::dark` handler in `wire.rs` — `theme::refresh`
/// then `App::restyle_all` — is what runs here, portal or no portal. Solarized raises no such
/// notify (it keeps the system's own dark state), so the pass is asked for the way
/// `Shell::apply_config` asks for it there.
///
/// Solarized follows the system between its two halves and Xvfb has no portal to move the system
/// with, so its dark half is a second launch with `ADW_DEBUG_COLOR_SCHEME=prefer-dark`.
///
/// `before` is read the instant the switch has been made and says what the tags still hold;
/// `after` is the same tags once the deferred pass has run. What to look for is `after`: every
/// foreground-derived tag must be that line's own `view_fg` at its alpha. When the pass ran
/// inside the notify they were the outgoing theme's instead — `listmarker=rgba(0,0,6,0.4)`,
/// near-black, against a `view_fg` of `rgb(255,255,255)`, which is the invisible bullet.
///
/// `scheme_text` is beside them to say why `view_fg` is the right thing to mix from. It is the
/// style scheme's own `text` foreground, and in none of the four themes is it what reaches the
/// glyphs: rendering the view and reading its pixels gives the prose as `view_fg` every time
/// (`(51,51,55)` on white under Adwaita, where the scheme says `#504E55`), so the scheme's ink is
/// a colour nothing on screen is drawn in.
pub(super) fn bench_theme(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        tab.set_text("plain prose\n- item\n> quote\n[link](x) `code`\n");
        for theme in [Theme::Light, Theme::Dark, Theme::Solarized] {
            crate::theme::apply(theme);
            app.restyle_all();
            println!("bench theme {theme:?} before {}", bench_theme_colours(&tab));
            glib::timeout_future(Duration::from_millis(300)).await;
            println!("bench theme {theme:?} after {}", bench_theme_colours(&tab));
        }
        bench_quit(&app);
    });
}

/// See `ACCENT_BENCH_NUMBERS` in `install_bench_hooks`.
pub(super) fn bench_numbers(app: &Rc<App>, rels: &str) {
    let rels: Vec<String> = rels.split(',').map(str::to_string).collect();
    let [first, code, later] = rels.as_slice() else {
        return bench_quit(app);
    };
    let (app, first, code, later) = (app.clone(), first.clone(), code.clone(), later.clone());
    app.open_path(&first);
    app.open_path(&code);
    glib::spawn_future_local(async move {
        let step = |what: &str| {
            let tabs: Vec<String> = app
                .open_tabs()
                .iter()
                .map(|tab| {
                    let (shown, width) = tab.line_numbers();
                    let front = match tab.view.is_mapped() {
                        true => "front",
                        false => "behind",
                    };
                    format!("{}:{shown}/{width}px/{front}", tab.rel())
                })
                .collect();
            println!("bench numbers {what} {}", tabs.join(" "));
        };
        // The switch itself, in the dialog the menu opens, which is then closed again.
        let flip = || {
            let _ = WidgetExt::activate_action(&app.window, "win.preferences", None);
            let row = find_widget(app.window.upcast_ref(), &|w| {
                w.downcast_ref::<adw::SwitchRow>()
                    .is_some_and(|row| row.title() == "Line Numbers")
            })
            .and_downcast::<adw::SwitchRow>();
            let Some(row) = row else {
                return println!("bench numbers no_switch");
            };
            row.set_active(!row.is_active());
            if let Some(dialog) = row.ancestor(adw::Dialog::static_type()) {
                dialog
                    .downcast::<adw::Dialog>()
                    .map(|d| d.force_close())
                    .ok();
            }
        };
        glib::timeout_future(Duration::from_millis(800)).await;
        // The note in front, the code behind it.
        app.open_path(&first);
        glib::timeout_future(Duration::from_millis(300)).await;
        step("opened");
        flip();
        glib::timeout_future(Duration::from_millis(300)).await;
        step("flipped");
        app.open_path(&later);
        glib::timeout_future(Duration::from_millis(800)).await;
        step("opened_after");
        app.open_path(&code);
        glib::timeout_future(Duration::from_millis(300)).await;
        step("code_in_front");
        flip();
        glib::timeout_future(Duration::from_millis(300)).await;
        step("flipped_back");
        app.open_path(&first);
        glib::timeout_future(Duration::from_millis(300)).await;
        step("note_in_front");
        // On again with both notes behind the code, then one of them brought forward.
        app.open_path(&code);
        glib::timeout_future(Duration::from_millis(300)).await;
        flip();
        glib::timeout_future(Duration::from_millis(300)).await;
        app.open_path(&later);
        glib::timeout_future(Duration::from_millis(300)).await;
        step("on_behind_then_front");
        // Off again by a hand edit to config.toml with the dialog up: the tabs and the dialog's
        // row both follow the file, and nothing is written back over it.
        let _ = WidgetExt::activate_action(&app.window, "win.preferences", None);
        let path = accent_core::config::config_path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::write(
            &path,
            text.replace("line_numbers = true", "line_numbers = false"),
        );
        let stamp = || {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&path)
                .map(|m| (m.ino(), m.mtime_nsec()))
                .ok()
        };
        let written = stamp();
        // Past the second a write accent owes the file would wait.
        glib::timeout_future(Duration::from_millis(2500)).await;
        let row = find_widget(app.window.upcast_ref(), &|w| {
            w.downcast_ref::<adw::SwitchRow>()
                .is_some_and(|row| row.title() == "Line Numbers")
        })
        .and_downcast::<adw::SwitchRow>();
        println!(
            "bench numbers hand_edit row={:?} rewritten={}",
            row.as_ref().map(|row| row.is_active()),
            stamp() != written
        );
        step("hand_edit");
        // The row built from the file is as live as the one it replaced.
        if let Some(row) = row {
            row.set_active(true);
            glib::timeout_future(Duration::from_millis(300)).await;
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            println!(
                "bench numbers row_after_edit writes={}",
                text.contains("line_numbers = true")
            );
        }
        bench_quit(&app);
    });
}

/// What the theme-derived tags of `tab` hold right now, each with what it reads at against the
/// page under it, next to what they are derived from: the view's resolved foreground, the scheme
/// the buffer is on and that scheme's own `text` ink.
fn bench_theme_colours(tab: &Rc<Tab>) -> String {
    let table = tab.buffer.tag_table();
    let page = crate::highlight::page(adw::StyleManager::default().is_dark());
    let says = |c: Option<gtk::gdk::RGBA>| match c {
        Some(c) => format!("{c}@{:.2}:1", crate::highlight::reads_at(c, page)),
        None => "none".to_string(),
    };
    let fg = |name: &str| says(table.lookup(name).and_then(|t| t.foreground_rgba()));
    let scheme = tab.buffer.style_scheme();
    format!(
        "dark={} scheme={:?} scheme_text={:?} page={page} view_fg={}@{:.2}:1 marker={} \
         listmarker={} quote={} taskdone={} code_bg={} link={} lanes={}",
        adw::StyleManager::default().is_dark(),
        scheme.as_ref().map(|s| s.id()),
        scheme
            .as_ref()
            .and_then(|s| s.style("text"))
            .and_then(|s| s.foreground()),
        tab.view.color(),
        crate::highlight::reads_at(tab.view.color(), page),
        fg("marker"),
        fg("listmarker"),
        fg("quote"),
        fg("taskdone"),
        says(table.lookup("code").and_then(|t| t.background_rgba())),
        fg("link"),
        // The wheel a CSV's columns and the git history's lanes share.
        (0..6)
            .map(|lane| says(Some(crate::highlight::lane_colour(lane))))
            .collect::<Vec<_>>()
            .join(","),
    )
}

pub(super) fn bench_occurrences(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
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
        let find = bench_search_tag(
            &tab,
            find_colour.as_deref(),
            Some(&tab.reveal_highlight().1),
        );
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
                bench_search_tag(
                    &tab,
                    find_colour.as_deref(),
                    Some(&tab.reveal_highlight().1)
                )
                .map(|tag| tag.priority()),
                muted.priority()
            );
            tab.buffer.insert(&mut tab.buffer.end_iter(), "al\n");
            bench_pump();
        }
        bench_quit(&app);
    });
}

/// Jump into the note at `rel` the three ways a jump arrives — a search hit's range, a tag's
/// name, a Go to Line — and print what the reveal painted each time, then what each kind of
/// interaction leaves of it. The find bar's query and whether it is painting are printed beside
/// it throughout: a jump hands the bar the matched text so `F3` steps through it, and that is all
/// it hands over — turning the highlight on with it is what used to light every other occurrence
/// in the note and never expire.
///
/// A click is not driven here because it is the same mark move an arrow key is: GtkTextView
/// places the caret on button-press, and the reveal comes down with the caret wherever it moves.
///
/// Point it at a note that carries a tag, or the first three lines have nothing to resolve.
pub(super) fn bench_reveal(app: &Rc<App>, rel: &str) {
    // The cold path first, which is the one a click in the sidebar really takes: the note has no
    // tab, so the jump waits on `with_tab` until one has landed and the load has filled it. The
    // note is read from disk here because nothing in the window has parsed it yet.
    let from_disk = std::fs::read_to_string(app.root().join(rel)).unwrap_or_default();
    match accent_core::markdown::analyze(&from_disk).tags.first() {
        Some(tag) => app.open_note_at(rel, Some(crate::sidebar::Target::Tag(tag.name.clone()))),
        None => app.open_path(rel),
    }
    let app = app.clone();
    let rel = rel.to_string();
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let under = |ranges: &[(i32, i32)]| {
            ranges.first().map(|&(from, to)| {
                tab.buffer
                    .text(
                        &tab.buffer.iter_at_offset(from),
                        &tab.buffer.iter_at_offset(to),
                        false,
                    )
                    .to_string()
            })
        };
        let (on, tag) = tab.reveal_highlight();
        let ranges = bench_tag_ranges(&tab, &tag);
        println!(
            "bench reveal case=cold_open on={on} at={ranges:?} text={:?}",
            under(&ranges)
        );

        // The same two targets again with the tab already open, which is the other half of
        // `with_tab`. Nothing here edits the buffer, so nothing is written back.
        let on_disk = tab.text();
        let tagged = accent_core::markdown::analyze(&on_disk)
            .tags
            .into_iter()
            .next();
        let tagged_name = tagged.as_ref().map(|t| t.name.clone());
        for (label, target) in [
            (
                "open_tag",
                tagged.map(|t| crate::sidebar::Target::Tag(t.name)),
            ),
            (
                "open_hit",
                // A word out of the note's own body, so what is printed under the reveal says
                // whether the byte range a search hit carries survived the count into characters.
                on_disk
                    .split_whitespace()
                    .find(|w| w.len() >= 6 && w.chars().all(|c| c.is_ascii_alphabetic()))
                    .and_then(|w| on_disk.find(w).map(|at| at..at + w.len()))
                    .map(crate::sidebar::Target::Range),
            ),
        ] {
            let Some(target) = target else {
                println!("bench reveal case={label} on=false at=[] text=None");
                continue;
            };
            app.open_note_at(&rel, Some(target));
            let (on, tag) = tab.reveal_highlight();
            let ranges = bench_tag_ranges(&tab, &tag);
            println!(
                "bench reveal case={label} on={on} at={ranges:?} text={:?}",
                under(&ranges)
            );
        }

        // A jump arriving while the bar is already open: the box has to end up saying what the tab
        // is now searching, and the write must not come back round as a search of the reader's
        // own — that would paint every match and step the caret off the place just revealed. The
        // box says `search-changed` twice for one write, the second after its delay, so the same
        // line is printed at the jump and again once that delay has passed.
        //
        // Before the note's text is replaced, and not after: the bar taking the keyboard is the
        // view losing it, which is one of the two things that autosave the buffer. A drill must
        // not write the vault it reads.
        let _ = WidgetExt::activate_action(&app.window, "win.find", None);
        bench_pump();
        let bar = app.pane().find.clone();
        let state = |label: &str, tab: &Rc<Tab>, bar: &Rc<crate::find::Bar>| {
            println!(
                "bench reveal {label} query={:?} painting={} on={} at={:?}",
                bar.query_text(),
                tab.is_highlighting(),
                tab.reveal_highlight().0,
                tab.buffer
                    .selection_bounds()
                    .map(|(s, e)| (s.offset(), e.offset()))
            );
        };
        state("bar_open", &tab, &bar);
        if let Some(name) = tagged_name {
            tab.goto_tag(&name);
        }
        state("bar_jump", &tab, &bar);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            state("bar_settled", &tab, &bar);
            // Go to Line paints the line it is pointing at while the number is still being typed,
            // the same way a hit is painted, and takes it down again when the box is emptied.
            bar.open(crate::find::Mode::Goto);
            // Line 1, which the note this drill wants has a heading on: an empty line has no
            // text to paint and would print the same `on=false` the bug did.
            for typed in ["1", ""] {
                bar.line_box().set_text(typed);
                let (on, tag) = tab.reveal_highlight();
                println!(
                    "bench reveal case=goto_preview typed={typed:?} on={on} at={:?}",
                    bench_tag_ranges(&tab, &tag)
                );
            }
            // The bar goes away again so the view has the keyboard back: from here on the drill
            // edits the buffer, and nothing may take the focus off a note that is dirty.
            bar.close();
            bench_reveal_marks(&app, &tab);
        });
    });
}

/// The second half of [`bench_reveal`], over text of the drill's own: what each kind of jump
/// paints, what each kind of interaction leaves of it, and the three match tags' priorities.
fn bench_reveal_marks(app: &Rc<App>, tab: &Rc<Tab>) {
    {
        let (app, tab) = (app.clone(), tab.clone());
        // ASCII throughout, so the byte offset `find` gives is also the character offset the
        // buffer counts in.
        let text = "Alpha beta alpha\ngamma #focus delta\nlast line here\n";
        tab.set_text(text);
        let (find_colour, muted_colour) = tab.match_colours();
        let (_, reveal_tag) = tab.reveal_highlight();
        println!(
            "bench reveal colours find={find_colour:?} reveal={:?} muted={muted_colour:?}",
            reveal_tag.background_rgba().map(|c| c.to_str().to_string())
        );
        let at = |needle: &str| text.find(needle).expect("bench needle") as i32;
        let show = |label: &str| {
            let (on, tag) = tab.reveal_highlight();
            println!(
                "bench reveal case={label} on={on} at={:?} query={:?} painting={}",
                bench_tag_ranges(&tab, &tag),
                Some(tab.find_query()).filter(|query| !query.is_empty()),
                tab.is_highlighting()
            );
        };

        // What a sidebar search result opens onto: the match itself, selected and revealed.
        let hit = at("beta") as usize;
        tab.goto_range(hit..hit + 4);
        show("search_hit");
        // What a row under a tag opens onto: where the note writes the tag, marker and all.
        tab.goto_tag("focus");
        show("tag");
        // What Go to Line lands on: the whole of the line, whatever column was asked for.
        tab.goto_line(3, 6);
        tab.reveal_line(3);
        show("goto_line");

        // `F3` straight after a jump, which is what the prefilled query is for: it steps from the
        // match the row pointed at to the next one, wrapping, without the bar ever being opened.
        let second = at("alpha") as usize;
        tab.goto_range(second..second + 5);
        tab.step(true, false);
        let stepped = tab
            .buffer
            .selection_bounds()
            .map(|(s, e)| (s.offset(), e.offset()));
        println!("bench reveal case=step_after_jump at={stepped:?}");

        // Each interaction in turn, the reveal put back between them.
        for (label, act) in [
            ("caret_move", 0),
            ("arrow_key", 1),
            ("keystroke", 2),
            ("edit_in_place", 3),
        ] {
            tab.goto_range(hit..hit + 4);
            match act {
                0 => tab.buffer.place_cursor(&tab.buffer.end_iter()),
                1 => tab
                    .view
                    .emit_move_cursor(gtk::MovementStep::VisualPositions, 1, false),
                2 => tab.buffer.insert_at_cursor("x"),
                // A delete forward leaves the caret where it is, so the edit is what answers.
                _ => {
                    let mut from = tab.buffer.end_iter();
                    let mut to = tab.buffer.end_iter();
                    from.backward_char();
                    tab.buffer.place_cursor(&from);
                    tab.goto_range(hit..hit + 4);
                    tab.buffer.delete(&mut from, &mut to);
                }
            }
            show(label);
        }

        // All three highlights on the same word. The find bar's tag has to outrank the reveal and
        // the reveal the muted hint, or a revealed match would be painted by the dimmer of them.
        // The bar's tag is found by its colour, which the reveal now shares, so the reveal is
        // taken out of the search before the one left is called the bar's.
        tab.set_text(text);
        tab.goto_range(hit..hit + 4);
        tab.set_query("beta");
        tab.set_highlight(true);
        bench_pump();
        let (_, reveal_tag) = tab.reveal_highlight();
        let (_, muted) = tab.occurrence_highlight();
        let find = bench_search_tag(&tab, find_colour.as_deref(), Some(&reveal_tag));
        println!(
            "bench reveal priority find={:?} reveal={} muted={} at={:?}",
            find.as_ref().map(|tag| tag.priority()),
            reveal_tag.priority(),
            muted.priority(),
            find.as_ref()
                .map(|tag| bench_tag_ranges(&tab, tag))
                .unwrap_or_default()
        );
        bench_quit(&app);
    }
}

/// The tag the find bar's search context paints with, found by its colour: gtksourceview keeps
/// that tag to itself, and the scheme's `search-match` background is what it was given. `skip` is
/// the reveal tag, which is given the same colour on purpose and would otherwise answer first.
fn bench_search_tag(
    tab: &Rc<Tab>,
    colour: Option<&str>,
    skip: Option<&gtk::TextTag>,
) -> Option<gtk::TextTag> {
    let wanted = gdk::RGBA::parse(colour?).ok()?;
    let mut found = None;
    tab.buffer.tag_table().foreach(|tag| {
        let mine = skip.is_some_and(|skip| skip == tag);
        if found.is_none()
            && !mine
            && tag.is_background_set()
            && tag.background_rgba() == Some(wanted)
        {
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

/// `ACCENT_BENCH_STYLE=listing:<rel>` opens the LaTeX file at `rel` and prints each line as runs
/// of the GtkSourceView context classes and the foreground the syntax colours give it
/// (`"def"[no-spell-check;#c64600]`, `-` where none does): what `latex.lang` makes of the listings
/// in it. Then `bench listing shot <theme>` under the default theme and Solarized, each held a
/// second and a half for a screenshot.
pub(super) fn bench_listing(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(600)).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let buffer = &tab.buffer;
        buffer.ensure_highlight(&buffer.start_iter(), &buffer.end_iter());
        println!(
            "bench listing language={:?}",
            buffer.language().map(|l| l.id())
        );
        for line in 0..buffer.line_count() {
            println!("bench listing {line} {}", bench_listing_runs(buffer, line));
        }
        for theme in [Theme::System, Theme::Solarized] {
            crate::theme::apply(theme);
            app.restyle_all();
            glib::timeout_future(Duration::from_millis(300)).await;
            println!("bench listing shot {theme:?}");
            glib::timeout_future(Duration::from_millis(1500)).await;
        }
        bench_quit(&app);
    });
}

/// One line of `buffer` as runs of equal context classes and syntax foreground.
fn bench_listing_runs(buffer: &sourceview5::Buffer, line: i32) -> String {
    let Some(mut iter) = buffer.iter_at_line(line) else {
        return String::new();
    };
    let look = |iter: &gtk::TextIter| {
        let fg = iter
            .tags()
            .iter()
            .filter_map(|tag| tag.foreground_rgba().filter(|_| tag.is_foreground_set()))
            .next_back()
            .map_or("-".to_string(), |c| {
                let byte = |v: f32| (v * 255.0).round() as u8;
                format!(
                    "#{:02x}{:02x}{:02x}",
                    byte(c.red()),
                    byte(c.green()),
                    byte(c.blue())
                )
            });
        (buffer.context_classes_at_iter(iter).join(","), fg)
    };
    let mut runs = Vec::new();
    let mut text = String::new();
    let mut current = look(&iter);
    while !iter.ends_line() {
        let now = look(&iter);
        if now != current {
            runs.push(format!("{text:?}[{};{}]", current.0, current.1));
            text.clear();
            current = now;
        }
        text.push(iter.char());
        iter.forward_char();
    }
    if !text.is_empty() {
        runs.push(format!("{text:?}[{};{}]", current.0, current.1));
    }
    runs.join(" ")
}
