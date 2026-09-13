//! Drills over comparisons: a note against its disk copy, two blobs, and the working tree.

use super::*;

/// The note is given fifty lines, written out, then edited in two places: a rewrite near the
/// top and a line added at the end. The comparison with the disk copy is then read back — rows,
/// hunks, hidden runs, buttons, how many rows GTK lays out at a height other than the one the
/// alignment asked for, and how much lower one column starts than the other (0 and 0 are the
/// claim) — before the first hunk is taken from Theirs, the hidden run is opened, and the same
/// is read again, with the button of the changed-on-disk banner that stands over it (none while
/// the comparison is up, Compare once it has gone). Then two blobs in a tab of their own, at a
/// zoom, for the same numbers and the page margins, which follow the zoom. With the vault under
/// git, last, the working tree against the index in the note's tab: whether it opened with the run
/// before the first change folded and the caret on that change, and then a character typed into
/// it, see [`bench_compare_type`]. That half wants a scratch repository whose committed note
/// differs from the fifty lines in a few places, one of them a long line where the drill writes a
/// short one, so the change is padded and the view has room to scroll. With that long line the
/// first, typing at the start of the change is typing at the start of the buffer, which is the one
/// place a padding tag has no newline before it.
pub(super) fn bench_compare(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        let body: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        tab.set_text(&body);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare write_failed {e}");
            return bench_quit(&app);
        }
        let (mut a, mut b) = (
            tab.buffer
                .iter_at_line(2)
                .unwrap_or_else(|| tab.buffer.end_iter()),
            tab.buffer
                .iter_at_line(3)
                .unwrap_or_else(|| tab.buffer.end_iter()),
        );
        tab.buffer.delete(&mut a, &mut b);
        tab.buffer.insert(&mut a, "line three\n");
        tab.buffer
            .insert(&mut tab.buffer.end_iter(), "added at the end\n");
        // The question a watcher raises when the note moves under unsaved edits, whose Compare
        // button is read while the comparison it opens is up and once it has gone.
        tab.show_alert(Alert::Compare);
        app.compare_with_disk(&tab);
        glib::timeout_add_local_once(Duration::from_millis(600), move || {
            let Some(compare) = tab.comparison() else {
                println!("bench compare none");
                return bench_quit(&app);
            };
            println!(
                "bench compare {} banner_button={:?}",
                bench_compare_line(&compare),
                bench_banner_button(&tab)
            );
            compare.take_hunk(0, false);
            compare.open_gap(0);
            glib::timeout_add_local_once(Duration::from_millis(300), move || {
                let line = tab
                    .buffer
                    .iter_at_line(2)
                    .map(|start| {
                        let mut end = start;
                        end.forward_to_line_end();
                        tab.buffer.text(&start, &end, true).to_string()
                    })
                    .unwrap_or_default();
                println!(
                    "bench compare_after {} line3={line:?}",
                    bench_compare_line(&compare)
                );
                tab.leave_compare();
                println!(
                    "bench compare_left comparing={} banner_button={:?}",
                    tab.comparison().is_some(),
                    bench_banner_button(&tab)
                );
                // Taken down again, so the rest of the drill runs with nothing standing.
                tab.clear_alert(Alert::Compare);
                let new = body.replace("line 10\n", "line ten\n");
                let diff = app.open_diff(
                    "diff:bench",
                    "bench.md",
                    "bench",
                    ("old", &body),
                    ("new", &new),
                );
                app.set_zoom(1.5);
                glib::timeout_add_local_once(Duration::from_millis(500), move || {
                    println!(
                        "bench compare_blobs {} margins={:?}",
                        bench_compare_line(diff.comparison()),
                        bench_margins(&diff.page.child())
                    );
                    // With the vault under git: the working tree against the index, in the
                    // note's tab, which the Git pane reaches through the same door as a row.
                    let Some(git) = app.git.get().filter(|git| git.has_repos()) else {
                        println!("bench compare_worktree no_repo");
                        return bench_quit(&app);
                    };
                    // Where a note that has just been opened has its caret.
                    tab.buffer.place_cursor(&tab.buffer.start_iter());
                    git.compare_worktree(&tab.rel());
                    glib::timeout_add_local_once(Duration::from_millis(800), move || {
                        let Some(compare) = tab.comparison() else {
                            println!("bench compare_worktree none");
                            return bench_quit(&app);
                        };
                        println!(
                            "bench compare_worktree title={:?} {} first={:?}",
                            tab.page.title(),
                            bench_compare_line(&compare),
                            compare.first_misaligned()
                        );
                        let caret = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
                        println!(
                            "bench compare_open leading_hidden={} caret_on_first_change={}",
                            compare.hides_row(0),
                            compare.opens_at() == Some(caret.offset())
                        );
                        let then = tab.clone();
                        bench_compare_type(&tab, 0.5, None, move || {
                            bench_compare_type(&then, 0.0, None, move || bench_quit(&app))
                        });
                    });
                });
            });
        });
    });
}

/// Type one character into the comparing editor's side — `line`, or the first change where it is
/// `None` — `at` of the way along that line, and print what moved while the comparison caught up:
/// how often the shared scroll range changed, whether the scroll position did, how many rows were
/// off right after the keystroke and once it had settled, and how often something laid over the
/// editor was hidden or shown. The claim is 0, false, 0, 0, 0. `padded` says whether the line
/// carried alignment padding, which is the case the flash was about, `padded_after` whether it
/// still carries it on the character just typed — the invariant the flash is the loss of — and
/// `scroll` is the position against the furthest it can go, which says whether it had room to
/// drift.
fn bench_compare_type(tab: &Rc<Tab>, at: f64, line: Option<i32>, then: impl FnOnce() + 'static) {
    let tab = tab.clone();
    glib::timeout_add_local_once(Duration::from_millis(300), move || {
        let (Some(compare), Some(adj)) = (tab.comparison(), tab.view.vadjustment()) else {
            return then();
        };
        let found = match line {
            Some(n) => tab.buffer.iter_at_line(n),
            None => compare.opens_at().map(|o| tab.buffer.iter_at_offset(o)),
        };
        let Some(line) = found else {
            println!("bench compare_type at={at} no_change");
            return then();
        };
        let padded = is_padded(&tab, &line);
        let mut end = line;
        end.forward_to_line_end();
        let offset = line.offset() + ((end.offset() - line.offset()) as f64 * at) as i32;
        // Kept as a number: the insert below invalidates every iter into the buffer.
        let number = line.line();
        let mark = tab.buffer.create_mark(None, &line, true);
        tab.view.scroll_to_mark(&mark, 0.0, true, 0.0, 0.5);
        tab.buffer.delete_mark(&mark);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let (value, moves, flips) = (adj.value(), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
            let count = |n: &Rc<Cell<u32>>| {
                let n = n.clone();
                move || n.set(n.get() + 1)
            };
            let tick = count(&moves);
            let mut ids = vec![(
                adj.clone().upcast::<glib::Object>(),
                adj.connect_upper_notify(move |_| tick()),
            )];
            // The overlaid buttons sit one level down, on the view's text child.
            let mut stack = vec![tab.view.clone().upcast::<gtk::Widget>()];
            while let Some(widget) = stack.pop() {
                let mut child = widget.first_child();
                while let Some(c) = child {
                    let tick = count(&flips);
                    ids.push((
                        c.clone().upcast(),
                        c.connect_visible_notify(move |_| tick()),
                    ));
                    child = c.next_sibling();
                    stack.push(c);
                }
            }
            // Through the caret, as a keystroke goes in: `diff::reclaim` reads it to know which
            // line was typed into.
            tab.buffer.place_cursor(&tab.buffer.iter_at_offset(offset));
            tab.buffer.insert_at_cursor("x");
            // Read back where the character went in, which is where GTK reads the line's spacing
            // from: a tag that began at the line's first character is behind it now.
            let padded_after = is_padded(&tab, &tab.buffer.iter_at_offset(offset));
            let now = compare.misaligned();
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                println!(
                    "bench compare_type at={at} line={number} padded={padded} padded_after={padded_after} upper_moves={} value_moved={} misaligned_now={now} misaligned={} flips={} scroll={value}/{}",
                    moves.get(),
                    adj.value() != value,
                    compare.misaligned(),
                    flips.get(),
                    adj.upper() - adj.page_size(),
                );
                for (object, id) in ids {
                    object.disconnect(id);
                }
                then();
            });
        });
    });
}

/// Ask for a comparison on a file that is not text — `binary.md`, which the drill's vault holds —
/// and print whether the ask ran and what the window said about it. The work used to be dropped
/// where the file turned out not to be text, leaving the reader the status page and no word about
/// the comparison they asked for.
fn bench_compare_binary(app: &Rc<App>, then: impl FnOnce() + 'static) {
    app.with_tab("binary.md", Opened::Preview, "compare", |_, _| {
        println!("bench compare_binary ran=true");
    });
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(600), move || {
        println!("bench compare_binary said={:?}", bench_said(&app));
        then();
    });
}

/// What a toast standing over the window reads, which is how a drill sees one: libadwaita gives
/// no way to ask the overlay what it is showing.
fn bench_said(app: &Rc<App>) -> Option<String> {
    let label = find_widget(app.window.upcast_ref(), &|w| {
        w.downcast_ref::<gtk::Label>()
            .is_some_and(|l| l.label().starts_with("Cannot "))
    })?;
    Some(label.downcast::<gtk::Label>().ok()?.label().to_string())
}

/// Whether the paragraph at `at` carries alignment padding above it, which is what GTK lays the
/// line out with and what a character typed at the line's start must not take away.
fn is_padded(tab: &Rc<Tab>, at: &gtk::TextIter) -> bool {
    let base = tab.view.pixels_above_lines();
    at.tags()
        .iter()
        .any(|t| t.is_pixels_above_lines_set() && t.pixels_above_lines() > base)
}

/// The left and top margins of the first text view under `widget`: the page a zoomed comparison
/// is laid out on.
fn bench_margins(widget: &gtk::Widget) -> Option<(i32, i32)> {
    if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
        return Some((view.left_margin(), view.top_margin()));
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(margins) = bench_margins(&c) {
            return Some(margins);
        }
        child = c.next_sibling();
    }
    None
}

/// The banner's button as it reads on screen: `None` for none.
fn bench_banner_button(tab: &Tab) -> Option<glib::GString> {
    tab.banner.button_label().filter(|label| !label.is_empty())
}

fn bench_compare_line(compare: &diff::Compare) -> String {
    let (rows, hunks, hidden, buttons) = compare.counts();
    format!(
        "rows={rows} hunks={hunks} hidden={hidden} buttons={buttons} misaligned={} skew={}",
        compare.misaligned(),
        compare.skew()
    )
}

/// The two places a padding tag does not begin at the newline before its line, which is where a
/// character typed at the line's start lands outside it and the line is laid out bare for a
/// frame: an empty first line, and a padded paragraph right under a padded blank line.
///
/// The note is written as three long paragraphs with unchanged lines around them and staged, then
/// the buffer is given an empty first line, an empty line 5 and a short line 6, so those three
/// rows are padded by three different amounts. A character is then typed at the start of line 1
/// and of line 6, and of the control line, a short line under an unchanged one whose padding tag
/// does begin at the newline before it; `padded_after` is the claim in all three.
///
/// It makes a repository in the vault root and stages the note, so point it at a throwaway vault.
/// The line of the control change, counting from 0: the three lines of context, the two changes
/// and the fourteen unchanged lines before it.
const CONTROL: i32 = 20;

pub(super) fn bench_compare_pads(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().next() else {
            return bench_quit(&app);
        };
        let (index, work) = pads_texts();
        tab.set_text(&index);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_pads write_failed {e}");
            return bench_quit(&app);
        }
        let rel = tab.rel();
        for args in [["init", "-q", ""], ["add", "--", rel.as_str()]] {
            let ok = std::process::Command::new("git")
                .args(args.iter().filter(|a| !a.is_empty()))
                .current_dir(app.root())
                .status()
                .is_ok_and(|s| s.success());
            println!("bench compare_pads git_{} {ok}", args[0]);
        }
        // The watcher's debounce and a repository discovery that runs git per directory.
        glib::timeout_add_local_once(Duration::from_millis(4000), move || {
            let Some(git) = app.git.get().filter(|git| git.has_repos()) else {
                println!("bench compare_pads no_repo");
                return bench_quit(&app);
            };
            tab.set_text(&work);
            git.compare_worktree(&rel);
            glib::timeout_add_local_once(Duration::from_millis(800), move || {
                let Some(compare) = tab.comparison() else {
                    println!("bench compare_pads none");
                    return bench_quit(&app);
                };
                let padded = |n: i32| {
                    tab.buffer
                        .iter_at_line(n)
                        .is_some_and(|at| is_padded(&tab, &at))
                };
                println!(
                    "bench compare_pads {} first_padded={} blank_padded={} under_blank_padded={} control_padded={}",
                    bench_compare_line(&compare),
                    padded(0),
                    padded(4),
                    padded(5),
                    padded(CONTROL)
                );
                let (then, last) = (tab.clone(), tab.clone());
                bench_compare_type(&tab, 0.0, Some(0), move || {
                    bench_compare_type(&then, 0.0, Some(5), move || {
                        bench_compare_type(&last, 0.0, Some(CONTROL), move || {
                            let quit = app.clone();
                            bench_compare_binary(&app, move || bench_quit(&quit));
                        })
                    })
                });
            });
        });
    });
}

/// The staged side and the buffer side of [`bench_compare_pads`]: four paragraphs that wrap to
/// four different heights, against an empty first line, an empty line, a short line under it, and
/// one last short line under an unchanged one — the control, whose padding tag does begin at the
/// newline before it.
fn pads_texts() -> (String, String) {
    let long = |n: usize, word: &str| vec![word; n].join(" ");
    let keep: String = (1..=14).map(|i| format!("keep {i}\n")).collect();
    let index = format!(
        "{}\nkeep one\nkeep two\nkeep three\n{}\n{}\n{keep}{}\nkeep last\n",
        long(60, "alpha"),
        long(36, "bravo"),
        long(18, "charlie"),
        long(24, "delta")
    );
    let work = format!("\nkeep one\nkeep two\nkeep three\n\nshort\n{keep}short too\nkeep last\n");
    (index, work)
}
