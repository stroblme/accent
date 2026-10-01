//! Drills over a note compared with its disk copy, and the tabs of two blobs some of them open
//! beside it.

use super::*;

/// The note is given fifty lines, written out, then edited in two places: a rewrite near the
/// top and a line added at the end. The comparison with the disk copy is then read back — rows,
/// hunks, hidden runs, buttons, how many rows GTK lays out at a height other than the one the
/// alignment asked for, and how much lower one column starts than the other (0 and 0 are the
/// claim) — before a Find Next into the hidden run, which has to open it on both sides, the first
/// hunk is taken from Theirs, the hidden run is opened, and the same is read again, with the
/// button of the changed-on-disk banner that stands over it (none while the comparison is up,
/// Compare once it has gone, which is the third Escape out of presentation with a find bar up).
/// Then two blobs in a tab of their own, at a
/// zoom, for the same numbers and the page margins, which follow the zoom. With the vault under
/// git, last, the working tree against the index in the note's tab: whether it opened with the run
/// before the first change folded and the caret on that change, and then a character typed into
/// it, see [`bench_compare_type`]. That half wants a scratch repository whose committed note
/// differs from the fifty lines in a few places, one of them a long line where the drill writes a
/// short one, which carries the difference as padding under it, so the change is padded and the
/// view has room to scroll.
pub(in crate::bench) fn bench_compare(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
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
            // Go to Line's preview into the run the comparison collapsed. It moves no caret, so
            // the comparison has to be laid again for the other side to open with it: a run still
            // counted hidden here is one opened on the editor's side alone.
            tab.show_line(25);
            println!(
                "bench compare_goto caret_line={} {}",
                tab.cursor_line(),
                bench_compare_line(&compare)
            );
            // Find Next into the same run, which by now is open: the caret's own way in.
            tab.buffer.place_cursor(&tab.buffer.start_iter());
            tab.set_query("line 25");
            tab.step(true, false);
            println!(
                "bench compare_find caret_line={} label={:?} {}",
                tab.cursor_line(),
                tab.matches_label(),
                bench_compare_line(&compare)
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
                // Escape as the window hears it when nothing closer to the focus took it: out of
                // presentation first, then the pane's find bar, and only the third stops comparing.
                let find = app.pane().find.clone();
                find.open(crate::find::Mode::Goto);
                app.set_presenting(true);
                println!(
                    "bench compare_present escaped={} presenting={} find_open={} comparing={}",
                    wire::escape_first(&app),
                    app.presenting.get().is_some(),
                    find.is_open(),
                    tab.comparison().is_some()
                );
                println!(
                    "bench compare_escape dismissed={} find_open={} comparing={}",
                    wire::dismiss(&app),
                    find.is_open(),
                    tab.comparison().is_some()
                );
                println!(
                    "bench compare_left dismissed={} comparing={} banner_button={:?}",
                    wire::dismiss(&app),
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
                            "bench compare_open leading_hidden={} caret_on_first_change={} \
                             first_hunk_on_screen={}",
                            compare.hides_row(0),
                            compare.opens_at() == Some(caret.offset()),
                            compare.first_hunk_on_screen()
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

/// The end-of-line diagnostics of a comparison's collapsed runs: they used to be drawn all the
/// same, one under the other on the single row the run stands for.
///
/// The file is given fifty lines, written out, then changed near the top and near the bottom so
/// the middle collapses, and handed four warnings — one on the changed line and three inside the
/// run that is about to be hidden. It prints how many messages each state put up: with the run
/// hidden the claim is 1, with it opened 4, and 4 again once the comparison has gone. The gutter
/// marks stay at 4 throughout, which is the icon on the left the hidden ones are left with.
///
/// Then the editor's own fold over the same file, which hides lines the same way: a block whose
/// header carries a warning of its own and whose two hidden lines carry one each. Nothing is
/// published between the fold and the reading, so what is printed is what folding alone laid: 2
/// while it is shut — the header's message and the one at the top of the file — and 4 once it is
/// open, the marks staying at 4 throughout.
///
/// Point it at a scratch text file no language server answers for — `n.txt` — since a publish
/// would replace what it hands over.
pub(in crate::bench) fn bench_compare_diag(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(400).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let body: String = (1..=50).map(|i| format!("line {i}\n")).collect();
        tab.set_text(&body);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_diag write_failed {e}");
            return bench_quit(&app);
        }
        tab.set_text(
            &body
                .replace("line 3\n", "line three\n")
                .replace("line 45\n", "line forty-five\n"),
        );
        // One on the changed line, three in the run between the two changes. Published again at
        // every step, as a server would: what a paint makes of them is what is being read.
        let items: Vec<_> = [2, 20, 21, 22]
            .map(|line| diagnostic(Severity::Warning, line, 0, 4))
            .to_vec();
        let say = |what: &str| {
            tab.set_diagnostics(items.clone());
            println!(
                "bench compare_diag {what} annotations={} marks={} {}",
                tab.annotated(),
                crate::diagnostics::painted(&tab.buffer).1,
                match tab.comparison() {
                    Some(compare) => bench_compare_line(&compare),
                    None => "comparing=false".to_string(),
                }
            );
        };
        // The same numbers with nothing published in between, which is what a fold has to lay
        // by itself.
        let stood = |what: &str| {
            println!(
                "bench compare_diag {what} annotations={} marks={}",
                tab.annotated(),
                crate::diagnostics::painted(&tab.buffer).1
            );
        };
        say("published");
        app.compare_with_disk(&tab);
        wait(800).await;
        let Some(compare) = tab.comparison() else {
            println!("bench compare_diag none");
            return bench_quit(&app);
        };
        say("collapsed");
        compare.open_gap(0);
        wait(400).await;
        say("opened");
        tab.leave_compare();
        wait(400).await;
        say("left");
        // A block whose header is the warned line 20 and whose body holds the other two.
        tab.set_folds(vec![Fold {
            start_line: 20,
            end_line: 22,
        }]);
        tab.toggle_fold(20);
        wait(400).await;
        stood("folded");
        tab.toggle_fold(20);
        wait(400).await;
        stood("unfolded");
        bench_quit(&app);
    });
}

/// A warning and a fold chevron on both kinds of padded line, for a screenshot of the gutter:
/// line 3, shorter than its partner, carries its blank below it, and line 8, under a paragraph
/// the buffer lacks, carries its blank above it. Prints each line's cell and first row as the
/// view lays them out; the icons belong beside the first row, as the line numbers are. Point it
/// at a scratch text file no language server answers for, as `diag:`. It holds the window up for
/// two seconds before it quits, for the screenshot.
pub(in crate::bench) fn bench_compare_gutter(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(400).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let long = ["a long paragraph that wraps"; 8].join(" ");
        let text = |three: &str, gone: Option<&str>| -> String {
            (1..=12)
                .flat_map(|i| {
                    let line = match i {
                        3 => three.to_string(),
                        _ => format!("line {i}"),
                    };
                    std::iter::once(line).chain(gone.filter(|_| i == 7).map(str::to_string))
                })
                .map(|line| line + "\n")
                .collect()
        };
        tab.set_text(&text(&long, Some(&long)));
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_gutter write_failed {e}");
            return bench_quit(&app);
        }
        tab.set_text(&text("line three", None));
        app.compare_with_disk(&tab);
        wait(800).await;
        tab.set_diagnostics(
            [2, 7]
                .map(|line| diagnostic(Severity::Warning, line, 0, 4))
                .to_vec(),
        );
        tab.set_folds(
            [2, 7]
                .map(|line| Fold {
                    start_line: line,
                    end_line: line + 1,
                })
                .to_vec(),
        );
        wait(800).await;
        for n in [2, 7] {
            let Some(at) = tab.buffer.iter_at_line(n) else {
                continue;
            };
            let ((top, height), row) = (tab.view.line_yrange(&at), tab.view.iter_location(&at));
            println!(
                "bench compare_gutter line={n} cell={top}+{height} first_row={}+{}",
                row.y(),
                row.height()
            );
        }
        wait(2000).await;
        bench_quit(&app);
    });
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

/// The conflict banner on `rel`, which can be any text file: a `*.sync-conflict-*` copy is
/// written beside it from outside the app while its tab is open, the tab is closed and opened
/// again, and the copy is removed, printing what the tab's banner stands for after each step —
/// the watcher's event, the question asked on opening, and the removal. It writes into the vault,
/// so point it at a scratch copy.
pub(in crate::bench) fn bench_compare_conflict(app: &Rc<App>, rel: &str) {
    let Some(vault) = app.vault().cloned() else {
        return bench_quit(app);
    };
    let (dir, name) = (accent_core::path::parent_dir(rel), doc::file_name(rel));
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    let copy = vault.root().join(dir).join(format!(
        "{stem}.sync-conflict-20260903-101500-ABCDEFG.{ext}"
    ));
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let alert = |app: &Rc<App>| app.tab_for(&rel).and_then(|tab| tab.alert());
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(800)).await;
        let _ = std::fs::write(&copy, "theirs\n");
        glib::timeout_future(Duration::from_secs(3)).await;
        println!("bench compare_conflict live={:?}", alert(&app));
        for doc in app.docs() {
            app.close_page(doc.page());
        }
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1500)).await;
        println!("bench compare_conflict reopened={:?}", alert(&app));
        let _ = std::fs::remove_file(&copy);
        glib::timeout_future(Duration::from_secs(3)).await;
        println!("bench compare_conflict removed={:?}", alert(&app));
        bench_quit(&app);
    });
}

/// The banner's button as it reads on screen: `None` for none.
fn bench_banner_button(tab: &Tab) -> Option<glib::GString> {
    tab.banner.button_label().filter(|label| !label.is_empty())
}

/// A note compared, the comparison scrolled and left once its overlay scrollbar has faded out, and
/// the editor scrolled the moment the other column has been freed: prints `gone=true` and the
/// scroll, for two comparisons. With the disk copy first, which puts the copy's column on the
/// editor's scrollbar: handing the column its own scrollbar back used to leave GTK's fade handler
/// for it on the editor's adjustment (see `diff::swap_vadjustment`), so this scroll ran the handler
/// on freed memory: a critical here (fatal under the drills' `G_DEBUG`), a segfault in a real
/// session, the window crash of 2026-09-28. Then with the index of a repository the drill makes in
/// the vault root, which puts the editor on the Index column's scrollbar: the editor's own
/// scrollbar kept the fade handler it was given before the comparison, and taking that scrollbar
/// back ran it on an indicator GTK had let go of, the same critical. Writes the note and makes a
/// repository, so point it at a throwaway vault.
pub(in crate::bench) fn bench_compare_left(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(400).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        // Enough lines to scroll, which the disk copy does not have.
        let lines = |word: &str| {
            (1..=200)
                .map(|i| format!("{word} {i}\n"))
                .collect::<String>()
        };
        tab.set_text(&lines("line"));
        app.compare_with_disk(&tab);
        println!("bench compare_left disk {}", bench_left(&tab).await);
        let root = app.root();
        for args in [
            &["init", "-q"][..],
            &["add", "--", &rel],
            &["commit", "-qm", "base"],
        ] {
            let _ = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=bench",
                    "-c",
                    "user.email=bench@accent.invalid",
                ])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&root)
                .output();
        }
        // The watcher's debounce and a repository discovery that runs git per directory.
        wait(4000).await;
        let Some(git) = app.git.get().filter(|git| git.has_repos()).cloned() else {
            println!("bench compare_left no_repo");
            return bench_quit(&app);
        };
        tab.set_text(&lines("changed"));
        git.compare_worktree(&rel);
        println!("bench compare_left worktree {}", bench_left(&tab).await);
        bench_quit(&app);
    });
}

/// Wait for `tab`'s comparison, scroll it half a page, leave it once its overlay scrollbar has
/// faded out, and scroll the editor the moment the other column is freed.
async fn bench_left(tab: &Tab) -> String {
    let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
    let mut column = None;
    for _ in 0..40 {
        wait(100).await;
        column = tab.comparison().and_then(|compare| {
            let views = [false, true].map(|end| pane_view(compare.widget(), end));
            let other = views
                .into_iter()
                .flatten()
                .find(|v| v != tab.view.upcast_ref::<gtk::TextView>())?;
            let adj = compare.vadjustment();
            adj.set_value(adj.value() + adj.page_size() / 2.0);
            Some(other.parent()?.downgrade())
        });
        if column.is_some() {
            break;
        }
    }
    let Some(column) = column else {
        return "none".to_string();
    };
    // Faded out two seconds after the last scroll, on a half-second tick.
    wait(3000).await;
    tab.leave_compare();
    for _ in 0..200 {
        if column.upgrade().is_none() {
            break;
        }
        wait(10).await;
    }
    let Some(adj) = tab.view.vadjustment() else {
        return "no_scroll".to_string();
    };
    let from = adj.value();
    adj.set_value(from + adj.page_size() / 2.0);
    format!(
        "gone={} scrolled={from}->{}",
        column.upgrade().is_none(),
        adj.value()
    )
}

/// The page a comparison's companion shares with the editor beside it. The note is a heading over
/// eighty lines, written out and then changed at line 70, so the comparison with its disk copy
/// hides the heading in the run it collapses and opens scrolled past it. Prints whether the sticky
/// block title is up (only the editor's column has one, and it showed there empty, over the first
/// row, its heading being hidden), and the `h1` marker's hang and the tab width on both sides
/// (`page=editor/companion`, the two equal) as the comparison opened and again after a zoom and a
/// new Indent Width: the companion hung its markers against a left margin of 0 and kept a tab width
/// of 4. Then the sticky title again once the comparison has gone and the editor is scrolled past
/// the heading. Writes the note, so point it at a scratch vault.
pub(in crate::bench) fn bench_compare_page(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(400).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let body: String = (1..=80).map(|i| format!("line {i}\n")).collect();
        tab.set_text(&format!("# Heading\n{body}"));
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_page write_failed {e}");
            return bench_quit(&app);
        }
        let changed = body.replace("line 70\n", "line seventy\n");
        tab.set_text(&format!("# Heading\n{changed}"));
        app.compare_with_disk(&tab);
        wait(1200).await;
        let Some(companion) = tab.comparison().and_then(|c| pane_view(c.widget(), true)) else {
            println!("bench compare_page none");
            return bench_quit(&app);
        };
        let page = |view: &gtk::TextView| {
            let hang = view.buffer().tag_table().lookup("hang1");
            let tabs = view
                .downcast_ref::<sourceview5::View>()
                .map(|v| v.tab_width());
            format!(
                "{:?},{tabs:?}",
                hang.map(|tag| (tag.left_margin(), tag.indent()))
            )
        };
        let say = |what: &str| {
            println!(
                "bench compare_page {what} sticky={} page={}/{}",
                tab.sticky_shown(),
                page(tab.view.upcast_ref()),
                page(&companion)
            )
        };
        tab.update_sticky();
        say("opened");
        app.set_zoom(1.5);
        tab.set_indent_width(2);
        wait(800).await;
        say("zoomed");
        tab.leave_compare();
        wait(300).await;
        if let Some(adj) = tab.view.vadjustment() {
            adj.set_value(adj.upper() / 2.0);
        }
        wait(300).await;
        println!("bench compare_page left sticky={}", tab.sticky_shown());
        bench_quit(&app);
    });
}

/// Hidden runs opened from their buttons, as a click does: the note is written as 400 lines, most
/// of them wrapping to a few rows, and changed at every fortieth from line 20, so the comparison
/// with its disk copy hides a run between each two and has pages of rows. The middle run's button
/// is pressed, then the first run's, the one above the first change at the top of the note, each
/// scrolled halfway down the view first. Prints for each where the scroll, the line above the
/// button and the line under it were before and after, in the view's pixels, and the furthest the
/// scroll strayed from where it was, read every 16 ms meanwhile (`value=a->b above=y0->y1
/// below=y0->y1 strayed=0`: the rows above stay and the run opens downwards), then the middle run
/// again in a tab of two blobs. Opening the first run used to scroll the view by twice the run's
/// height, which both views took back from the scroll they share. The blobs are then read again
/// with three lines more at the top of each side, as the Git pane's refresh reads them, printing
/// the line at the top of the view and where it starts before and after (`reread
/// top="line 18"@-4->"line 18"@-4`, the two the same): the view used to land elsewhere. Writes the
/// note, so point it at a scratch vault.
pub(in crate::bench) fn bench_compare_gap(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(400).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let text = |changed: bool| -> String {
            (1..=400)
                .map(|i| match changed && i % 40 == 20 {
                    true => format!("line {i} changed\n"),
                    false => format!("line {i} {}\n", "wrapping words ".repeat(i % 7 * 5)),
                })
                .collect()
        };
        let (disk, edited) = (text(false), text(true));
        tab.set_text(&disk);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_gap write_failed {e}");
            return bench_quit(&app);
        }
        tab.set_text(&edited);
        app.compare_with_disk(&tab);
        wait(1200).await;
        let Some(compare) = tab.comparison() else {
            println!("bench compare_gap none");
            return bench_quit(&app);
        };
        let view: &gtk::TextView = tab.view.upcast_ref();
        println!(
            "bench compare_gap middle {}",
            bench_gap(&compare, view, None).await
        );
        println!(
            "bench compare_gap first {}",
            bench_gap(&compare, view, Some(0)).await
        );
        tab.leave_compare();
        let diff = app.open_diff(
            "diff:gap",
            "gap.md",
            "gap",
            ("old", &disk),
            ("new", &edited),
        );
        wait(1200).await;
        let compare = diff.comparison();
        let Some(view) = pane_view(compare.widget(), true) else {
            println!("bench compare_gap blobs none");
            return bench_quit(&app);
        };
        println!(
            "bench compare_gap blobs {}",
            bench_gap(compare, &view, None).await
        );
        // The Git pane's refresh reading both blobs again, each three lines longer at the top.
        let before = top(&view);
        let longer = |text: &str| format!("new 1\nnew 2\nnew 3\n{text}");
        diff.set_texts(&longer(&disk), &longer(&edited));
        wait(800).await;
        println!("bench compare_gap reread top={before}->{}", top(&view));
        bench_quit(&app);
    });
}

/// The line at the top of `view`, its first eight characters, and how far above the top its row
/// starts.
fn top(view: &gtk::TextView) -> String {
    let seen = view.visible_rect();
    let (at, y) = view.line_at_y(seen.y());
    let mut end = at;
    end.forward_to_line_end();
    let text: String = view
        .buffer()
        .text(&at, &end, true)
        .chars()
        .take(8)
        .collect();
    format!("{text:?}@{}", y - seen.y())
}

/// Show All Unchanged Lines pressed and let go, over a note of 400 lines changed at every
/// fortieth compared with its disk copy, the middle run opened from its button first and that
/// button's row put halfway down the view; then over a tab of two blobs. Prints the controls in
/// the toggle's title row by tooltip (`row=["Show All Unchanged Lines", "Stop Comparing"]`), and
/// for each press the hidden runs and the line at the top of the view before and after
/// (`on hidden=10->0 top="line 181"@-250->"line 181"@-250`): every run opens, and letting go
/// hides them all again, the one opened by hand too, the line at the top staying where it was
/// in every frame painted meanwhile (`away=0/…`, see [`painted`]). Then at the start of the file,
/// where the first run opens downwards from the top of the view and collapses again under it
/// (`start on … top="line 17 "@24->"line 1 w"@24`), and last the two blobs read again three lines
/// longer at the top, as the Git pane's refresh reads them (`reread`). Writes the note, so point it
/// at a scratch vault.
pub(in crate::bench) fn bench_compare_unfold(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(400).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let text = |changed: bool| -> String {
            (1..=400)
                .map(|i| match changed && i % 40 == 20 {
                    true => format!("line {i} changed\n"),
                    false => format!("line {i} {}\n", "wrapping words ".repeat(i % 7 * 5)),
                })
                .collect()
        };
        let (disk, edited) = (text(false), text(true));
        tab.set_text(&disk);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_unfold write_failed {e}");
            return bench_quit(&app);
        }
        tab.set_text(&edited);
        app.compare_with_disk(&tab);
        wait(1200).await;
        let Some(compare) = tab.comparison() else {
            println!("bench compare_unfold none");
            return bench_quit(&app);
        };
        let view: &gtk::TextView = tab.view.upcast_ref();
        let Some(views) = both_columns(&compare, view) else {
            println!("bench compare_unfold columns none");
            return bench_quit(&app);
        };
        let buttons = overlaid(view, "⋯");
        if let Some((y, button)) = buttons.get(buttons.len() / 2).cloned() {
            centre(&compare, view, y).await;
            button.emit_clicked();
            wait(800).await;
        }
        println!("bench compare_unfold editor {}", bench_row(&compare));
        for on in [true, false] {
            println!(
                "bench compare_unfold editor {}",
                bench_unfold(&compare, &views, on).await
            );
        }
        // The run over the file's first lines collapses again under the top of the view.
        compare.vadjustment().set_value(0.0);
        wait(500).await;
        for on in [true, false] {
            println!(
                "bench compare_unfold start {}",
                bench_unfold(&compare, &views, on).await
            );
        }
        tab.leave_compare();
        let diff = app.open_diff(
            "diff:unfold",
            "unfold.md",
            "unfold",
            ("old", &disk),
            ("new", &edited),
        );
        wait(1200).await;
        let compare = diff.comparison();
        let Some(views) = pane_view(compare.widget(), true).and_then(|v| both_columns(compare, &v))
        else {
            println!("bench compare_unfold blobs none");
            return bench_quit(&app);
        };
        println!("bench compare_unfold blobs {}", bench_row(compare));
        for on in [true, false] {
            println!(
                "bench compare_unfold blobs {}",
                bench_unfold(compare, &views, on).await
            );
        }
        // The Git pane's refresh reading both blobs again, each three lines longer at the top,
        // which keeps the line at the top of the view the same way.
        let longer = |text: &str| format!("new 1\nnew 2\nnew 3\n{text}");
        let reread = painted(&views, || diff.set_texts(&longer(&disk), &longer(&edited))).await;
        println!("bench compare_unfold reread {reread}");
        bench_quit(&app);
    });
}

/// The comparison's Show All Unchanged Lines toggle.
fn unfold_toggle(compare: &diff::Compare) -> Option<gtk::ToggleButton> {
    find_widget(compare.widget(), &|w| w.is::<gtk::ToggleButton>())?
        .downcast()
        .ok()
}

/// The tooltips of the controls in the toggle's title row, in order.
fn bench_row(compare: &diff::Compare) -> String {
    let Some(row) = unfold_toggle(compare).and_then(|t| t.parent()) else {
        return "row=none".to_string();
    };
    let mut tips = Vec::new();
    let mut child = row.first_child();
    while let Some(c) = child {
        tips.extend(c.tooltip_text().map(|t| t.to_string()));
        child = c.next_sibling();
    }
    format!("row={tips:?}")
}

/// `view` and the comparison's other column, in that order.
fn both_columns(compare: &diff::Compare, view: &gtk::TextView) -> Option<[gtk::TextView; 2]> {
    let other = [true, false]
        .into_iter()
        .filter_map(|end| pane_view(compare.widget(), end))
        .find(|other| other != view)?;
    Some([view.clone(), other])
}

/// Press (`on`) or let go of the toggle and say how the hidden runs moved, and the lines at the top
/// of `views` (see [`painted`]).
async fn bench_unfold(compare: &diff::Compare, views: &[gtk::TextView; 2], on: bool) -> String {
    let Some(toggle) = unfold_toggle(compare) else {
        return "toggle=none".to_string();
    };
    let hidden = compare.counts().2;
    let tops = painted(views, || toggle.set_active(on)).await;
    let what = if on { "on" } else { "off" };
    format!(
        "{what} hidden={hidden}->{} {tops} sensitive={}",
        compare.counts().2,
        toggle.is_sensitive()
    )
}

/// Run `act` and say where the line at the top of the first of `views` was before and is once
/// fifty frames have gone by, and in how many of the frames painted meanwhile (`frames`) the line
/// at the top of each view was neither where it was before nor where it ends
/// (`away=<first>/<other>`): a view shown somewhere else on the way. It used to be both, for the
/// frames GTK took to lay out the lines above.
async fn painted(views: &[gtk::TextView; 2], act: impl FnOnce()) -> String {
    let tops = || views.each_ref().map(top);
    let before = tops();
    let seen: Rc<RefCell<Vec<[String; 2]>>> = Rc::default();
    let clock = views[0].frame_clock();
    let id = clock.as_ref().map(|clock| {
        let (seen, views) = (seen.clone(), views.clone());
        clock.connect_after_paint(move |_| seen.borrow_mut().push(views.each_ref().map(top)))
    });
    act();
    for _ in 0..50 {
        glib::timeout_future(Duration::from_millis(16)).await;
    }
    if let (Some(clock), Some(id)) = (clock, id) {
        clock.disconnect(id);
    }
    let after = tops();
    let seen = seen.borrow();
    let away = [0, 1].map(|i| {
        let elsewhere = |tops: &&[String; 2]| tops[i] != before[i] && tops[i] != after[i];
        seen.iter().filter(elsewhere).count()
    });
    format!(
        "top={}->{} frames={} away={}/{}",
        before[0],
        after[0],
        seen.len(),
        away[0],
        away[1]
    )
}

/// Scroll the button of the hidden run `pick` on `view` (the middle one for `None`) halfway down
/// the view, as far as the view goes, press it as a pointer does, the focus and then `clicked`, and
/// say where the scroll and the lines above and under the button were before and after, and how far
/// the scroll strayed meanwhile.
async fn bench_gap(compare: &diff::Compare, view: &gtk::TextView, pick: Option<usize>) -> String {
    let adj = compare.vadjustment();
    let buttons = overlaid(view, "⋯");
    let Some((top, button)) = buttons.get(pick.unwrap_or(buttons.len() / 2)).cloned() else {
        return format!("buttons={}", buttons.len());
    };
    centre(compare, view, top).await;
    // Kept as marks, since the click lays both buffers again. The button sits in the padding of
    // the line under the run; the line above is the visible one before that, if there is one.
    let buffer = view.buffer();
    let (below, _) = view.line_at_y(top);
    let mut above = below;
    let marks = [above.backward_visible_line().then_some(above), Some(below)]
        .map(|at| at.map(|at| buffer.create_mark(None, &at, true)));
    let ys = || {
        marks.clone().map(|mark| {
            mark.map(|mark| {
                let y = view.iter_location(&buffer.iter_at_mark(&mark)).y();
                view.buffer_to_window_coords(gtk::TextWindowType::Widget, 0, y)
                    .1
            })
        })
    };
    let (value, before) = (adj.value(), ys());
    button.grab_focus();
    button.emit_clicked();
    let mut strayed = 0.0_f64;
    for _ in 0..50 {
        glib::timeout_future(Duration::from_millis(16)).await;
        strayed = strayed.max((adj.value() - value).abs());
    }
    let after = ys();
    for mark in marks.into_iter().flatten() {
        buffer.delete_mark(&mark);
    }
    format!(
        "value={value}->{} above={:?}->{:?} below={:?}->{:?} strayed={strayed}",
        adj.value(),
        before[0],
        after[0],
        before[1],
        after[1]
    )
}

/// The shown buttons laid over `view` whose label starts with `label`, top to bottom, each with
/// the `y` it starts at in buffer coordinates.
fn overlaid(view: &gtk::TextView, label: &str) -> Vec<(i32, gtk::Button)> {
    let mut buttons = Vec::new();
    let mut stack = vec![view.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = stack.pop() {
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            if let Ok(button) = c.clone().downcast::<gtk::Button>()
                && button.is_visible()
                && button.label().is_some_and(|l| l.starts_with(label))
            {
                let at = button.compute_point(view, &gtk::graphene::Point::zero());
                buttons.push((
                    at.map_or(0, |p| p.y() as i32) + view.visible_rect().y(),
                    button,
                ));
            }
            stack.push(c);
        }
    }
    buttons.sort_by_key(|(y, _)| *y);
    buttons
}

/// Scroll the comparison so buffer `y` of `view` is halfway down it, as far as it goes.
async fn centre(compare: &diff::Compare, view: &gtk::TextView, y: i32) {
    let (adj, seen) = (compare.vadjustment(), view.visible_rect());
    adj.set_value(adj.value() + f64::from(y - seen.y()) - adj.page_size() / 2.0);
    glib::timeout_future(Duration::from_millis(500)).await;
}

/// The overlaid buttons pressed through the real pointer, which a drill's `clicked` is not: a note
/// of 400 lines changed at every fortieth, compared with its disk copy, the editor's caret at its
/// start. Prints `bench compare_press aim <x> <y>` over the middle "⋯" button of the editor's
/// column, for `build-aux/xtest.py :N "move <x> <y>; focus; down; up"`, and once the run has
/// opened (`acted=true`), the editor's caret line and the widget with the keyboard; then the same
/// over the middle Take button on the other column, with that column's caret offset. The claim is
/// `caret_line=0` and `theirs_caret=0` throughout, the keyboard staying where it was: the press
/// reached the text view under the button too, which put that column's caret under the pointer
/// and gave it the keyboard. Waits
/// ten seconds for each press. Writes the note, so point it at a scratch vault.
pub(in crate::bench) fn bench_compare_press(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(400).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let text = |changed: bool| -> String {
            (1..=400)
                .map(|i| match changed && i % 40 == 20 {
                    true => format!("line {i} changed\n"),
                    false => format!("line {i}\n"),
                })
                .collect()
        };
        tab.set_text(&text(false));
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_press write_failed {e}");
            return bench_quit(&app);
        }
        tab.set_text(&text(true));
        app.compare_with_disk(&tab);
        wait(1200).await;
        let (Some(compare), Some(theirs)) = (
            tab.comparison(),
            tab.comparison().and_then(|c| pane_view(c.widget(), true)),
        ) else {
            println!("bench compare_press none");
            return bench_quit(&app);
        };
        tab.buffer.place_cursor(&tab.buffer.start_iter());
        let mine: &gtk::TextView = tab.view.upcast_ref();
        let say = |what: &str, acted: bool| {
            let caret = |view: &gtk::TextView| {
                let buffer = view.buffer();
                buffer.iter_at_mark(&buffer.get_insert())
            };
            let focus = GtkWindowExt::focus(&app.window).map(|w| w.type_().name().to_string());
            println!(
                "bench compare_press {what} acted={acted} caret_line={} theirs_caret={} focus={focus:?}",
                caret(mine).line(),
                caret(&theirs).offset()
            );
        };
        for (what, view, label) in [("gap", mine, "⋯"), ("take", &theirs, "Take")] {
            let buttons = overlaid(view, label);
            let Some((top, button)) = buttons.get(buttons.len() / 2).cloned() else {
                println!("bench compare_press {what} none");
                continue;
            };
            centre(&compare, view, top).await;
            let Some(root) = button.root() else { continue };
            let middle = gtk::graphene::Point::new(
                button.width() as f32 / 2.0,
                button.height() as f32 / 2.0,
            );
            let (sx, sy) = app.window.surface_transform();
            if let Some(p) = button.compute_point(&root, &middle) {
                println!(
                    "bench compare_press aim {:.0} {:.0}",
                    f64::from(p.x()) + sx,
                    f64::from(p.y()) + sy
                );
            }
            let counts = compare.counts();
            for _ in 0..100 {
                wait(100).await;
                if compare.counts() != counts {
                    break;
                }
            }
            wait(300).await;
            say(what, compare.counts() != counts);
        }
        bench_quit(&app);
    });
}

/// A note with its first section folded, compared with its disk copy, which differs only at the
/// end: the run the comparison collapses reaches over the fold's end. It asks for the iter at every
/// pixel row of the editor, as GtkSourceView asks at the top and bottom of the screen on every
/// frame, and prints whether the fold was open meanwhile and is shut again once the comparison
/// has gone. With the fold left shut, the line where it ended was laid out as a blank row, and an
/// iter asked in that row's pixels-below-lines aborted the process ("Byte index … is off the end of
/// the line"). Writes the note, so point it at a scratch vault.
pub(in crate::bench) fn bench_compare_folds(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(400).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let section = |name: &str| -> String {
            let body: String = (1..=10).map(|i| format!("{name} line {i}\n")).collect();
            format!("# {name}\n{body}")
        };
        tab.set_text(&format!("{}{}", section("One"), section("Two")));
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_folds write_failed {e}");
            return bench_quit(&app);
        }
        tab.set_folds(vec![
            Fold {
                start_line: 0,
                end_line: 10,
            },
            Fold {
                start_line: 11,
                end_line: 21,
            },
        ]);
        tab.toggle_fold(0);
        let mut last = tab.buffer.end_iter();
        last.backward_char();
        tab.buffer.insert(&mut last, " changed");
        let folded = |tab: &Tab| crate::fold::is_folded(tab.buffer.upcast_ref(), 0);
        let before = folded(&tab);
        app.compare_with_disk(&tab);
        for _ in 0..40 {
            wait(100).await;
            if tab.comparison().is_some() {
                break;
            }
        }
        wait(500).await;
        let Some(compare) = tab.comparison() else {
            println!("bench compare_folds none");
            return bench_quit(&app);
        };
        let during = folded(&tab);
        let (y, height) = tab.view.line_yrange(&tab.buffer.end_iter());
        for y in 0..y + height {
            tab.view.iter_at_location(0, y);
        }
        println!(
            "bench compare_folds shut_before={before} shut_during={during} {}",
            bench_compare_line(&compare)
        );
        drop(compare);
        tab.leave_compare();
        println!("bench compare_folds shut_after={}", folded(&tab));
        bench_quit(&app);
    });
}
