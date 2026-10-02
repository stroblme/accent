//! Drills over the comparisons the Git pane's rows open: the working tree, the index, a commit.

use super::*;

/// A file opened the way the Git pane opens one: its Changes row activated, with no tab holding
/// the file yet. Prints which section the row was in, whether a tab came up comparing, and what
/// the comparison holds. Point it at a repository whose `<rel>` is modified in the working tree.
///
/// Three of them are the bug this covers, each a row that is older than what git now says, so
/// both sides carry the same text. `stale:<rel>` stages the file behind the pane's back and
/// activates the Changes row that has not caught up; the comparison it used to open had
/// `hunks=0` — two identical columns, while the editor's own gutter went on marking the change
/// against HEAD. `staged:<rel>` stages it, waits for the Staged row, unstages behind the pane's
/// back and activates that row. `commit:<rel>` asks for the file at HEAD against HEAD~1, where
/// HEAD did not touch it — the shape a commit's file list has once history has moved under it.
/// All three now say so and ask git again instead of opening a tab of two identical columns.
///
/// Pointed at a file HEAD did change, `commit:` prints whether the tab opened with its first change
/// on screen, and the shared scrollbar's value, upper and page size: a long file whose one change
/// is deep inside used to open scrolled away from it, with everything around it folded.
///
/// Every comparison that opens is then scrolled half a page down and read back once it has had
/// time to lay itself again (`scrolled want=… got=…`, the two equal): a file with one side empty
/// — untracked, newly staged, deleted, or added by the commit — went back to `got=0`, the empty
/// column's view pulling the scroll they share into its own few pixels. In the note's own tab the
/// minimap is switched on first and its scroll printed either side (`map=…->…`), then again across
/// a page of the editor's own scroll once the comparison is left (`left map=…->…`): it stood still
/// while the editor was on the companion's scrollbar. Point it at a note whose changes keep most
/// rows on screen, or the minimap has nothing to scroll. `stale:` opens the file
/// first and prints whether its editor has its own scrollbar back once the comparison has been
/// left (`own_scroll=true`), then scrolls it: the editor used to keep the companion's adjustment,
/// and with it the freed companion's handler, and that scroll crashed the window — every time
/// under `MALLOC_PERTURB_=165`.
pub(in crate::bench) fn bench_compare_row(app: &Rc<App>, rel: &str) {
    app.show_pane("git");
    let (mode, rel) = match rel.split_once(':') {
        Some((mode @ ("stale" | "staged" | "commit"), rel)) => (mode, rel),
        _ => ("", rel),
    };
    let (app, mode, rel) = (app.clone(), mode.to_string(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        let root = app.root();
        let git_cmd = move |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .unwrap_or_default()
        };
        // The debounced refresh and its `git status`, and on a remote vault the host's answer.
        wait(2500).await;
        for _ in 0..120 {
            if app.git.get().is_some_and(|git| git.changes_rows() > 0) {
                break;
            }
            wait(250).await;
        }
        let Some(git) = app.git.get().filter(|git| git.has_repos()).cloned() else {
            println!("bench compare_row no_repo");
            return bench_quit(&app);
        };
        // A toast already up, the first index's, would hold the row's own back in the queue,
        // where nothing can read it.
        for _ in 0..80 {
            if bench_toast(&app).is_none() {
                break;
            }
            wait(100).await;
        }
        // A Staged row needs one refresh to exist before it can be made stale.
        if mode == "staged" {
            git_cmd(&["add", "--", &rel]);
            wait(2500).await;
        }
        // The file's own scrollbar, from before the comparison its outgrown row begins and leaves.
        let own = match mode.as_str() {
            "stale" => {
                app.open_path(&rel);
                let mut own = None;
                for _ in 0..40 {
                    wait(250).await;
                    let tab = app.open_tabs().into_iter().find(|tab| tab.rel() == rel);
                    own = tab.and_then(|tab| tab.view.vadjustment());
                    if own.is_some() {
                        break;
                    }
                }
                own
            }
            _ => None,
        };
        match mode.as_str() {
            "stale" => println!(
                "bench compare_row staged={:?}",
                git_cmd(&["add", "--", &rel])
            ),
            "staged" => println!(
                "bench compare_row unstaged={:?}",
                git_cmd(&["restore", "--staged", "--", &rel])
            ),
            _ => {}
        }
        let said = app.toasted.get();
        let key = match mode.as_str() {
            "commit" => {
                // Abbreviated to the seven characters the tab key is built from, so the key
                // below is the one the comparison would open under.
                let oid = git_cmd(&["rev-parse", "--short=7", "HEAD"]);
                let parent = git_cmd(&["rev-parse", "--short=7", "HEAD~1"]);
                println!("bench compare_row commit oid={oid:?} parent={parent:?}");
                git.compare_commit(&rel, &oid, &parent);
                format!("diff:commit:{oid}:{rel}")
            }
            _ => {
                println!(
                    "bench compare_row rows={} section={:?}",
                    git.changes_rows(),
                    git.activate_change(&rel)
                );
                format!("diff:index:{rel}")
            }
        };
        // Both sides are read on a worker, over the wire on a remote vault; then the comparison
        // settles.
        for _ in 0..60 {
            wait(250).await;
            if app.toasted.get() > said
                || matches!(app.doc_for(&key), Some(Doc::Diff(_)))
                || app.open_tabs().iter().any(|tab| tab.comparison().is_some())
            {
                break;
            }
        }
        wait(1500).await;
        let tabs = app.open_tabs();
        let comparing = tabs.iter().find_map(|tab| Some((tab, tab.comparison()?)));
        match (comparing, app.doc_for(&key)) {
            (Some((tab, compare)), _) => {
                println!(
                    "bench compare_row opened rel={:?} {}",
                    tab.rel(),
                    bench_compare_line(&compare)
                );
                println!("bench compare_row tops {}", bench_tops(&compare));
                // The minimap, switched on over the comparison, follows the scroll both columns
                // share, and the editor's own once the comparison is left.
                tab.set_minimap(true);
                wait(300).await;
                let map = bench_map(tab);
                println!(
                    "bench compare_row scrolled {} map={map:?}->{:?}",
                    bench_scroll(&compare).await,
                    bench_map(tab)
                );
                tab.leave_compare();
                wait(300).await;
                let map = bench_map(tab);
                if let Some(adj) = tab.view.vadjustment() {
                    adj.set_value(adj.value() + adj.page_size());
                }
                wait(500).await;
                println!("bench compare_row left map={map:?}->{:?}", bench_map(tab));
            }
            // A Staged row and a commit's file open a tab of two read-only panes instead, which
            // has to open scrolled to the first change.
            (None, Some(Doc::Diff(diff))) => {
                let compare = diff.comparison();
                let adj = compare.vadjustment();
                println!(
                    "bench compare_row opened key={key:?} {} first_hunk_on_screen={} \
                     scroll={}/{}/{}",
                    bench_compare_line(compare),
                    compare.first_hunk_on_screen(),
                    adj.value(),
                    adj.upper(),
                    adj.page_size()
                );
                println!("bench compare_row tops {}", bench_tops(compare));
                println!("bench compare_row scrolled {}", bench_scroll(compare).await);
            }
            // The refusal is a toast, and it asks git again, so the row it refused goes too.
            (None, _) => {
                println!(
                    "bench compare_row opened tabs={:?} comparing=false toasts={} said={:?}",
                    tabs.iter().map(|tab| tab.rel()).collect::<Vec<_>>(),
                    app.toasted.get() - said,
                    bench_toast(&app)
                );
                if let Some(own) = own
                    && let Some(adj) = tabs
                        .iter()
                        .find(|tab| tab.rel() == rel)
                        .and_then(|tab| tab.view.vadjustment())
                {
                    let own_scroll = adj == own;
                    adj.set_value(adj.value() + adj.page_size() / 2.0);
                    wait(300).await;
                    println!(
                        "bench compare_row refused own_scroll={own_scroll} scrolled={}",
                        adj.value()
                    );
                }
            }
        }
        bench_quit(&app);
    });
}

/// Half a page further down the comparison, as a wheel turn goes, and where the shared scrollbar
/// is once the comparison has had time to lay itself again.
async fn bench_scroll(compare: &diff::Compare) -> String {
    let adj = compare.vadjustment();
    let want = (adj.value() + adj.page_size() / 2.0).min(adj.upper() - adj.page_size());
    adj.set_value(want);
    glib::timeout_future(Duration::from_millis(500)).await;
    format!("want={want} got={}", adj.value())
}

/// Where the minimap's own scroll is while it is on, which has to follow the editor's.
fn bench_map(tab: &Tab) -> Option<f64> {
    let map = tab.minimap().downcast_ref::<sourceview5::Map>()?;
    map.is_visible()
        .then(|| map.vadjustment())
        .flatten()
        .map(|adj| adj.value())
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

/// Stage, Unstage and Revert Selected Lines, end to end. It makes a repository in the vault root
/// and commits the note as twelve lines, so point it at a throwaway vault. The tab then rewrites
/// line 3 and adds a line under line 9, and the working tree is compared with the index; line 3
/// is selected in the editor and staged, then selected in the Index pane of the staged comparison
/// and unstaged. It prints the entries each pane's menu offers and what the index holds after
/// each step. Then the added line is selected in the editor and reverted, which takes it out of
/// the buffer, the index untouched, and an undo puts it back (`reverted gone=true … undone=true`).
/// Last it leaves line 3 selected in the working-tree comparison for eight seconds, for
/// `build-aux/xtest.py` to open the menu on with a secondary click (`hold` says when).
pub(in crate::bench) fn bench_compare_lines(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(400).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let base: String = (1..=12).map(|i| format!("line {i}\n")).collect();
        tab.set_text(&base);
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_lines write_failed {e}");
            return bench_quit(&app);
        }
        let root = app.root();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=bench",
                    "-c",
                    "user.email=bench@accent.invalid",
                ])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&root)
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
                .unwrap_or_default()
        };
        git(&["init", "-q"]);
        git(&["add", "--", &rel]);
        git(&["commit", "-qm", "base"]);
        // The watcher's debounce and a repository discovery that runs git per directory.
        wait(4000).await;
        let Some(panel) = app.git.get().filter(|git| git.has_repos()).cloned() else {
            println!("bench compare_lines no_repo");
            return bench_quit(&app);
        };
        let edited = base
            .replace("line 3\n", "line three\n")
            .replace("line 9\n", "line 9\nline 9b\n");
        tab.set_text(&edited);
        panel.compare_worktree(&rel);
        wait(800).await;
        let Some(compare) = tab.comparison() else {
            println!("bench compare_lines none");
            return bench_quit(&app);
        };
        let index = format!(":{rel}");
        println!(
            "bench compare_lines worktree menus={:?} {}",
            [
                pane_view(compare.widget(), false),
                Some(tab.view.clone().upcast())
            ]
            .map(|v| v.map(labels)),
            bench_compare_line(&compare)
        );
        select_line(&tab.buffer, 2);
        let _ = tab.view.activate_action("diff.stage", None);
        wait(1500).await;
        println!(
            "bench compare_lines staged index={:?} {}",
            git(&["show", &index]),
            bench_compare_line(&compare)
        );

        panel.compare_staged(&rel);
        wait(800).await;
        let Some(Doc::Diff(staged)) = app.doc_for(&format!("diff:index:{rel}")) else {
            println!("bench compare_lines no_staged_tab");
            return bench_quit(&app);
        };
        let views = [false, true].map(|end| pane_view(staged.comparison().widget(), end));
        println!(
            "bench compare_lines staged_tab menus={:?} {}",
            views.clone().map(|v| v.map(labels)),
            bench_compare_line(staged.comparison())
        );
        if let [_, Some(view)] = views {
            select_line(&view.buffer(), 2);
            let _ = view.activate_action("diff.unstage", None);
        }
        wait(1500).await;
        println!(
            "bench compare_lines unstaged index={:?} {}",
            git(&["show", &index]),
            bench_compare_line(staged.comparison())
        );

        app.reveal_page(&tab.page);
        wait(800).await;
        select_line(&tab.buffer, 9);
        let _ = tab.view.activate_action("diff.revert", None);
        wait(300).await;
        let has_9b = |tab: &Tab| tab.text().contains("line 9b");
        println!(
            "bench compare_lines reverted gone={} index={:?} {}",
            !has_9b(&tab),
            git(&["show", &index]),
            bench_compare_line(&compare)
        );
        tab.buffer.undo();
        wait(300).await;
        println!("bench compare_lines undone={}", has_9b(&tab));

        select_line(&tab.buffer, 2);
        println!("bench compare_lines hold");
        wait(8000).await;
        bench_quit(&app);
    });
}

/// The entries of the section a view adds to GTK's own context menu, first.
fn labels(view: gtk::TextView) -> Vec<String> {
    let Some(section) = view
        .extra_menu()
        .and_then(|menu| menu.item_link(0, "section"))
    else {
        return Vec::new();
    };
    (0..section.n_items())
        .filter_map(|i| {
            section
                .item_attribute_value(i, "label", None)?
                .get::<String>()
        })
        .collect()
}

/// Select line `n` (from 0) whole, its newline included.
fn select_line(buffer: &impl IsA<gtk::TextBuffer>, n: i32) {
    let buffer = buffer.as_ref();
    if let (Some(start), Some(end)) = (buffer.iter_at_line(n), buffer.iter_at_line(n + 1)) {
        buffer.select_range(&start, &end);
    }
}

/// Where the first hunk's lines and the ones after it start on each side: `row:old/new`, which a
/// pair of lines has level.
fn bench_tops(compare: &diff::Compare) -> String {
    let tops: Vec<String> = compare
        .first_hunk_tops()
        .into_iter()
        .map(|(r, old, new)| format!("{r}:{old:?}/{new:?}"))
        .collect();
    tops.join(" ")
}

/// Typing at the start of a padded line, where the character lands outside a padding tag that
/// begins at it and the line is laid out bare for a frame, and the one place such a tag does not
/// begin at the newline before its line: a padded paragraph right under a padded blank line.
///
/// A changed line shorter than its partner carries the difference as padding under it. The note
/// is written as five long paragraphs with unchanged lines around them and staged, then the buffer
/// makes the first three a short line, an empty one and another short one, each padded by its own
/// amount, and the last two paragraphs two short lines, the second of them the control, under a
/// padded line that is not blank. A character is then typed at the start of the empty line, of
/// the line under it and of the control; `padded_after` is the claim in all three.
///
/// It makes a repository in the vault root and stages the note, so point it at a throwaway vault.
/// The lines, counting from 0: three unchanged lines before the empty one, and the fourteen
/// unchanged lines and the short line before the control.
const BLANK: i32 = 4;
const CONTROL: i32 = 21;

pub(in crate::bench) fn bench_compare_pads(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
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
                    "bench compare_pads {} blank_padded={} under_blank_padded={} control_padded={}",
                    bench_compare_line(&compare),
                    padded(BLANK),
                    padded(BLANK + 1),
                    padded(CONTROL)
                );
                let (then, last) = (tab.clone(), tab.clone());
                bench_compare_type(&tab, 0.0, Some(BLANK), move || {
                    bench_compare_type(&then, 0.0, Some(BLANK + 1), move || {
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

/// The staged side and the buffer side of [`bench_compare_pads`]: five paragraphs that wrap to
/// five different heights, against a short line, an empty line and a short line under it, and two
/// last short lines, the second the control, whose padding tag does begin at the newline before it.
fn pads_texts() -> (String, String) {
    let long = |n: usize, word: &str| vec![word; n].join(" ");
    let keep: String = (1..=14).map(|i| format!("keep {i}\n")).collect();
    let index = format!(
        "keep one\nkeep two\nkeep three\n{}\n{}\n{}\n{keep}{}\n{}\nkeep last\n",
        long(60, "alpha"),
        long(36, "bravo"),
        long(18, "charlie"),
        long(24, "delta"),
        long(30, "echo")
    );
    let work = format!(
        "keep one\nkeep two\nkeep three\nshort\n\nshort two\n{keep}short too\nshort three\nkeep last\n"
    );
    (index, work)
}

/// Typing by real keys into a working-tree comparison of 400 lines, the editor being its right
/// column: changed at every fortieth line from line 20 and staged, so a run hides between each two
/// changes. The lines wrap to a few rows each, which puts the note above the 16 KB under which the
/// editor lays the comparison again on each keystroke rather than on its debounce; `short:` makes
/// them one or two rows, under it. The middle run is opened from its button and XTEST types into it
/// and around it, each step asked for as `bench compare_typing xtest <steps>`: `inside` three
/// letters at the end of its middle line, `back` three BackSpaces that make that line unchanged
/// again, `newline` a Return there and `join` a BackSpace that takes the new line out again,
/// `above` and `below` three letters on the changed line before and after the run; then three
/// letters in the middle of the run above it, opened by a jump into it as a search hit or the
/// outline makes (`jump`), and of the last run, opened from its button with the view at the end of
/// the file (`end`). For each it prints the hidden runs before and after, whether the run opened is
/// still open, and over every frame painted until a second and a half after the last key: where the
/// line typed into starts below the top of the view before and after (`line_y`), in how many frames
/// it was anywhere else (`jumped`) or its partner in the other column off its row (`skewed`), in
/// how many some row on screen had its two lines at different heights once the edit was laid over
/// (`uneven`, the first such row as `row:old_y/new_y`), and in how many either view had to skip
/// drawing (`blank`, see `fold::aborts_at`). Makes a repository in the vault root and stages the
/// note, so point it at a throwaway vault.
pub(in crate::bench) fn bench_compare_typing(app: &Rc<App>, rel: &str) {
    let (short, rel) = match rel.strip_prefix("short:") {
        Some(rel) => (true, rel),
        None => (false, rel),
    };
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
                    false if short => format!("line {i} {}\n", "word ".repeat(i % 7)),
                    false => format!("line {i} {}\n", "wrapping words ".repeat(i % 7 * 5)),
                })
                .collect()
        };
        tab.set_text(&text(false));
        if let Err(e) = app.write_tab(&tab, None) {
            println!("bench compare_typing write_failed {e}");
            return bench_quit(&app);
        }
        for args in [["init", "-q", ""], ["add", "--", rel.as_str()]] {
            std::process::Command::new("git")
                .args(args.iter().filter(|a| !a.is_empty()))
                .current_dir(app.root())
                .status()
                .ok();
        }
        // The watcher's debounce and a repository discovery that runs git per directory.
        wait(4000).await;
        let Some(git) = app.git.get().filter(|git| git.has_repos()) else {
            println!("bench compare_typing no_repo");
            return bench_quit(&app);
        };
        tab.set_text(&text(true));
        git.compare_worktree(&rel);
        for _ in 0..200 {
            wait(50).await;
            if tab.comparison().is_some_and(|c| c.settled()) {
                break;
            }
        }
        let (Some(compare), Some(theirs)) = (
            tab.comparison(),
            tab.comparison().and_then(|c| pane_view(c.widget(), false)),
        ) else {
            println!("bench compare_typing none");
            return bench_quit(&app);
        };
        println!("bench compare_typing xtest move 300 15; focus");
        for _ in 0..100 {
            if app.window.is_active() {
                break;
            }
            wait(100).await;
        }
        let Some((first, last)) = open_run(&tab, &compare, None).await else {
            return bench_quit(&app);
        };
        let middle = (first + last) / 2;
        let cases = [
            ("inside", middle, middle, "type abc", 3),
            (
                "back",
                middle,
                middle,
                "key BackSpace; key BackSpace; key BackSpace",
                3,
            ),
            ("newline", middle, middle, "key Return", 1),
            ("join", middle, middle + 1, "key BackSpace", 1),
            ("above", first - 4, first - 4, "type abc", 3),
            ("below", last + 4, last + 4, "type abc", 3),
        ];
        let open = |first: i32, last: i32| (first..=last).all(|n| !gap_hides(&tab, n));
        for (name, track, caret, steps, keys) in cases {
            let runs = compare.counts().2;
            let typed = typed(&tab, &compare, &theirs, (track, caret), steps, keys).await;
            println!(
                "bench compare_typing {name} hidden={runs}->{} open={} {typed}",
                compare.counts().2,
                open(first, last)
            );
        }
        // The run above, by a jump into it as a search hit or the outline makes, and the last.
        let Some(above) = (0..first).rev().find(|&n| gap_hides(&tab, n)) else {
            return bench_quit(&app);
        };
        let jump = tab.buffer.iter_at_line(above - 8).expect("bench line");
        for name in ["jump", "end"] {
            let run = match name {
                "jump" => opened(&tab, || tab.jump_to(&jump, 0.5)).await,
                _ => open_run(&tab, &compare, Some(usize::MAX)).await,
            };
            let Some((first, last)) = run else {
                return bench_quit(&app);
            };
            let (middle, runs) = ((first + last) / 2, compare.counts().2);
            let typed = typed(&tab, &compare, &theirs, (middle, middle), "type abc", 3).await;
            println!(
                "bench compare_typing {name} hidden={runs}->{} open={} {typed}",
                compare.counts().2,
                open(first, last)
            );
        }
        bench_quit(&app);
    });
}

/// Whether line `n` of `tab` is in a comparison's hidden run.
fn gap_hides(tab: &Tab, n: i32) -> bool {
    let gap = tab.buffer.tag_table().lookup(diff::TAG_GAP);
    tab.buffer
        .iter_at_line(n)
        .zip(gap)
        .is_some_and(|(at, gap)| at.has_tag(&gap))
}

/// Press the button of the hidden run `pick` on the editor's column (the middle one for `None`,
/// the last for anything past it), its row halfway down the view, and say which lines it opened.
async fn open_run(tab: &Tab, compare: &diff::Compare, pick: Option<usize>) -> Option<(i32, i32)> {
    let view = tab.view.upcast_ref::<gtk::TextView>();
    let buttons = disk::overlaid(view, "⋯");
    let at = pick
        .unwrap_or(buttons.len() / 2)
        .min(buttons.len().saturating_sub(1));
    let (y, button) = buttons.get(at).cloned()?;
    disk::centre(compare, view, y).await;
    opened(tab, || button.emit_clicked()).await
}

/// Run `open` and say which hidden lines it opened, first and last.
async fn opened(tab: &Tab, open: impl FnOnce()) -> Option<(i32, i32)> {
    let shut: Vec<i32> = (0..tab.buffer.line_count())
        .filter(|&n| gap_hides(tab, n))
        .collect();
    open();
    glib::timeout_future(Duration::from_millis(800)).await;
    let opened: Vec<i32> = shut.into_iter().filter(|&n| !gap_hides(tab, n)).collect();
    let run = (*opened.first()?, *opened.last()?);
    println!("bench compare_typing opened={}..={}", run.0, run.1);
    Some(run)
}

/// What a frame showed: where the line typed into and its partner start below the top of their
/// views, the first row on screen whose two lines were at different heights, and whether a view
/// skipped drawing.
type Frame = (i32, i32, Option<String>, bool);

/// Put the caret at the end of line `at.1`, halfway down the view, ask for `steps` by XTEST, and
/// say what every frame painted until a second and a half after the `keys`th change showed of line
/// `at.0`: see [`bench_compare_typing`]. Its partner in `theirs` is the line of the same number,
/// which every case leaves it. Halfway down, so that GTK has no reason to scroll to the caret.
async fn typed(
    tab: &Rc<Tab>,
    compare: &Rc<diff::Compare>,
    theirs: &gtk::TextView,
    at: (i32, i32),
    steps: &str,
    keys: u32,
) -> String {
    let (buffer, mine) = (&tab.buffer, tab.view.upcast_ref::<gtk::TextView>());
    let (Some(line), Some(partner), Some(mut caret)) = (
        buffer.iter_at_line(at.0),
        theirs.buffer().iter_at_line(at.0),
        buffer.iter_at_line(at.1),
    ) else {
        return "line=none".to_string();
    };
    if !caret.ends_line() {
        caret.forward_to_line_end();
    }
    buffer.place_cursor(&caret);
    disk::centre(compare, mine, mine.iter_location(&caret).y()).await;
    tab.view.grab_focus();
    let marks = [
        buffer.create_mark(None, &line, true),
        theirs.buffer().create_mark(None, &partner, true),
    ];
    let sample = {
        let (views, marks, compare) = (
            [mine.clone(), theirs.clone()],
            marks.clone(),
            compare.clone(),
        );
        move || {
            let [line, partner] = [0, 1].map(|i| {
                let (view, mark) = (&views[i], &marks[i]);
                view.iter_location(&view.buffer().iter_at_mark(mark)).y() - view.visible_rect().y()
            });
            let blank = views.iter().any(|view| {
                let seen = view.visible_rect();
                [seen.y(), seen.y() + seen.height()]
                    .into_iter()
                    .any(|y| crate::fold::aborts_at(view, y))
            });
            let uneven = compare.uneven().filter(|(n, _)| *n > 0).map(|(_, row)| row);
            (line, partner, uneven, blank)
        }
    };
    let before = sample();
    let seen: Rc<RefCell<Vec<Frame>>> = Rc::default();
    let Some(clock) = mine.frame_clock() else {
        return "clock=none".to_string();
    };
    let painted = clock.connect_after_paint({
        let (seen, sample) = (seen.clone(), sample.clone());
        move |_| seen.borrow_mut().push(sample())
    });
    let changes = Rc::new(Cell::new(0));
    let changed = buffer.connect_changed({
        let changes = changes.clone();
        move |_| changes.set(changes.get() + 1)
    });
    println!("bench compare_typing xtest {steps}");
    for _ in 0..100 {
        if changes.get() >= keys {
            break;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    glib::timeout_future(Duration::from_millis(1500)).await;
    clock.disconnect(painted);
    buffer.disconnect(changed);
    let (after, seen) = (sample(), seen.take());
    buffer.delete_mark(&marks[0]);
    theirs.buffer().delete_mark(&marks[1]);
    let count = |f: &dyn Fn(&Frame) -> bool| seen.iter().filter(|s| f(s)).count();
    let first = seen.iter().find_map(|s| s.2.clone()).unwrap_or_default();
    format!(
        "line={} keys={}/{keys} frames={} line_y={}->{} jumped={} skewed={} uneven={}{} blank={}",
        at.0,
        changes.get(),
        seen.len(),
        before.0,
        after.0,
        count(&|s| s.0 != before.0),
        count(&|s| s.0 != s.1),
        count(&|s| s.2.is_some()),
        if first.is_empty() {
            String::new()
        } else {
            format!("@{first}")
        },
        count(&|s| s.3),
    )
}

/// A Changes row clicked right after another repository is picked (`=pick:<rel>`). Point it at a
/// vault whose root repository has `<rel>` changed and a second repository beside it: the drill
/// picks the second one in the chooser and activates `<rel>`'s row at once, before the refresh
/// the pick asks for has landed, and prints whether that row was still listed and, for whatever
/// comparison opened, both sides' line counts. The list used to go on showing the root's rows
/// until then, and the row compared its path in the repository just picked, where git has no
/// such file: an empty Index side and the whole file drawn as added, deletions and all. Then it
/// picks the root again, lets the refresh land, and prints the same for a click on the row.
pub(in crate::bench) fn bench_compare_pick(app: &Rc<App>, rel: &str) {
    app.show_pane("git");
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        // Discovery finds the second repository after the first refresh.
        for _ in 0..120 {
            wait(250).await;
            if app.git.get().is_some_and(|git| git.repo_names().len() > 1) {
                break;
            }
        }
        let Some(git) = app.git.get().cloned() else {
            return bench_quit(&app);
        };
        println!("bench compare_pick repos={:?}", git.repo_names());
        for (at, settle) in [(1, 0), (0, 2500)] {
            git.select_repo(at);
            wait(settle).await;
            let listed = git.activate_change(&rel).is_some();
            wait(1500).await;
            let sides = app
                .open_tabs()
                .into_iter()
                .find(|tab| tab.rel() == rel)
                .and_then(|tab| tab.comparison())
                .map(|compare| {
                    let lines =
                        |end| pane_view(compare.widget(), end).map(|v| v.buffer().line_count());
                    format!("old_lines={:?} new_lines={:?}", lines(false), lines(true))
                });
            println!("bench compare_pick picked={at} listed={listed} {sides:?}");
            if let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) {
                tab.leave_compare();
            }
        }
        bench_quit(&app);
    });
}

/// Changes, Staged, Deleted and history rows clicked in the orders and at the moments a reader
/// clicks them (`=clicks`): a file not open yet, the same row again, a file open in a tab, right
/// after a save, right after the file changed on disk under its open tab, while a refresh is on
/// its way, two rows a few milliseconds apart, a deletion, a staged change and a commit's file.
/// After each it prints what the comparison in front holds once it has settled: each column's
/// line count, whether that is the text git has for it (`ok`), how many of its lines show text on
/// screen (`seen`) and each column's width, and `bad` names what is off. A comparison showing one
/// side only has a column empty, wrong, off screen or squeezed. It makes a repository in the
/// vault root, so point it at a throwaway vault.
pub(in crate::bench) fn bench_compare_clicks(app: &Rc<App>) {
    app.show_pane("git");
    let app = app.clone();
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        let root = app.root();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=bench",
                    "-c",
                    "user.email=bench@accent.invalid",
                ])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(&root)
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
                .unwrap_or_default()
        };
        let write = |rel: &str, text: &str| {
            let _ = std::fs::write(root.join(rel), text);
        };
        let (long, open, gone, staged) = (
            note(40, "long"),
            note(12, "open"),
            note(4, "gone"),
            note(6, "staged"),
        );
        let big: String = (1..=1500)
            .map(|i| format!("    let value_{i} = compute(\"a line of code that is long enough to wrap in a narrow column {i}\", {i});\n"))
            .collect();
        for (rel, text) in [
            ("long.md", &long),
            ("open.md", &open),
            ("gone.md", &gone),
            ("staged.md", &staged),
            ("big.rs", &big),
        ] {
            write(rel, text);
        }
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        // A second commit for the history row.
        let before = long.clone();
        let long = long.replace("## long 3\n", "## long three\n");
        write("long.md", &long);
        git(&["commit", "-qam", "three"]);
        let long_work = long
            .replace("## long 1\n", "## long one\n")
            .replace("- long item 20 a\n", "")
            .replace("## long 30\n", "## long thirty\nA paragraph added.\n\n")
            + "added at the end\n";
        let open_work = open.replace("## open 5\n", "## open five\n");
        let staged_index = staged.replace("## staged 2\n", "## staged two\n");
        let big_work: String = big
            .lines()
            .enumerate()
            .filter(|(i, _)| !(400..700).contains(i))
            .map(|(i, l)| match i % 37 {
                0 => format!("{l} // changed\n"),
                _ => format!("{l}\n"),
            })
            .collect::<String>()
            + &"    added();\n".repeat(200);
        write("big.rs", &big_work);
        write("long.md", &long_work);
        write("open.md", &open_work);
        write("staged.md", &staged_index);
        let _ = std::fs::remove_file(root.join("gone.md"));
        git(&["add", "staged.md"]);
        let (oid, parent) = (
            git(&["rev-parse", "--short=7", "HEAD"]),
            git(&["rev-parse", "--short=7", "HEAD~1"]),
        );
        for _ in 0..120 {
            wait(250).await;
            if app.git.get().is_some_and(|git| git.changes_rows() >= 5) {
                break;
            }
        }
        let Some(panel) = app.git.get().filter(|git| git.has_repos()).cloned() else {
            println!("bench compare_clicks no_repo");
            return bench_quit(&app);
        };
        for _ in 0..80 {
            if bench_toast(&app).is_none() {
                break;
            }
            wait(100).await;
        }
        println!("bench compare_clicks rows={}", panel.changes_rows());
        let typed = RefCell::new(String::new());
        let expect = |rel: &str| -> (String, String) {
            match rel {
                "big.rs" => (big.clone(), big_work.clone()),
                "long.md" => (long.clone(), long_work.clone()),
                "open.md" => (open.clone(), open_work.clone() + &typed.borrow()),
                "gone.md" => (gone.clone(), String::new()),
                _ => (staged.clone(), staged_index.clone()),
            }
        };
        let click = |rel: &str| {
            if panel.activate_change(rel).is_none() {
                println!("bench compare_clicks unlisted rel={rel}");
            }
        };
        let report = |case: &str, rel: &str, expect: (String, String)| {
            let (app, case, rel) = (app.clone(), case.to_string(), rel.to_string());
            async move {
                glib::timeout_future(Duration::from_millis(1500)).await;
                println!(
                    "bench compare_clicks {case} rel={rel} {}",
                    columns(&app, &rel, &expect)
                );
            }
        };

        // The first click on a file no tab holds, a long source file and a note, and the same
        // row again over its comparison.
        click("big.rs");
        report("first", "big.rs", expect("big.rs")).await;
        click("long.md");
        report("first", "long.md", expect("long.md")).await;
        click("long.md");
        report("again", "long.md", expect("long.md")).await;
        // A file already open in a tab of its own, plain.
        app.open_path("open.md");
        wait(800).await;
        click("open.md");
        report("open", "open.md", expect("open.md")).await;
        // Right after a save, whose refresh lands while the comparison is read.
        let tab = app
            .open_tabs()
            .into_iter()
            .find(|tab| tab.rel() == "open.md");
        if let Some(tab) = &tab {
            tab.leave_compare();
            tab.buffer.insert(&mut tab.buffer.end_iter(), "typed\n");
            typed.borrow_mut().push_str("typed\n");
            let _ = app.write_tab(tab, None);
            click("open.md");
            report("saved", "open.md", expect("open.md")).await;
        }
        // Right after the file changed on disk under its open tab, which reloads on the watcher's
        // news.
        if let Some(tab) = &tab {
            tab.leave_compare();
            wait(800).await;
            typed.borrow_mut().push_str("from disk\n");
            write("open.md", &expect("open.md").1);
            click("open.md");
            report("disk", "open.md", expect("open.md")).await;
        }
        // A refresh on its way: an untracked file appears as the row is clicked.
        write("new.md", "new\n");
        click("long.md");
        report("refreshing", "long.md", expect("long.md")).await;
        // Two rows a few milliseconds apart: the second is what is left in front.
        for ms in [0, 5, 20, 60, 150] {
            for (a, b) in [
                ("long.md", "open.md"),
                ("open.md", "long.md"),
                ("gone.md", "long.md"),
                ("long.md", "staged.md"),
            ] {
                click(a);
                wait(ms).await;
                click(b);
                report(&format!("quick{ms}:{a}"), b, expect(b)).await;
            }
        }
        click("gone.md");
        report("deleted", "gone.md", expect("gone.md")).await;
        click("staged.md");
        report("staged", "staged.md", expect("staged.md")).await;
        // And straight back to a file that is open, from a tab of two blobs.
        click("long.md");
        report("back", "long.md", expect("long.md")).await;
        // A commit's file, and the file again.
        panel.compare_commit("long.md", &oid, &parent);
        report("commit", "long.md", (before, long.clone())).await;
        click("long.md");
        report("after_commit", "long.md", expect("long.md")).await;
        bench_quit(&app);
    });
}

/// A note of `n` sections as a reader writes one: a heading, a paragraph long enough to wrap, a
/// list and now and then a fenced block, each line naming `name` and its section.
fn note(n: usize, name: &str) -> String {
    (1..=n)
        .map(|i| {
            let words = format!("{name} {i} ").repeat(24);
            let fence = match i % 4 {
                0 => format!("```rust\nfn {name}_{i}() {{}}\n```\n\n"),
                _ => String::new(),
            };
            format!(
                "## {name} {i}\n\n{words}\n\n- {name} item {i} a\n- {name} item {i} b\n\n{fence}"
            )
        })
        .collect()
}

/// What the comparison in front for `rel` shows: see [`bench_compare_clicks`].
fn columns(app: &Rc<App>, rel: &str, expect: &(String, String)) -> String {
    let front = app.tabs().selected_page().map(|page| page.title());
    let compare = app
        .open_tabs()
        .into_iter()
        .find(|tab| tab.rel() == rel)
        .and_then(|tab| tab.comparison())
        .or_else(|| {
            let front = app.tabs().selected_page()?;
            app.docs().into_iter().find_map(|doc| match doc {
                Doc::Diff(diff)
                    if diff.page == front && diff.key().ends_with(&format!(":{rel}")) =>
                {
                    Some(diff.comparison().clone())
                }
                _ => None,
            })
        });
    let Some(compare) = compare else {
        return format!(
            "front={front:?} comparing=false toast={:?}",
            bench_toast(app)
        );
    };
    let paned = compare.widget().downcast_ref::<gtk::Paned>().cloned();
    let mut bad = Vec::new();
    let mut widths = [0; 2];
    let mut side = |end: bool| {
        let Some(view) = pane_view(compare.widget(), end) else {
            return "none".to_string();
        };
        let buffer = view.buffer();
        let (s, e) = buffer.bounds();
        let text = buffer.text(&s, &e, true);
        let want = if end { &expect.1 } else { &expect.0 };
        let width = paned
            .as_ref()
            .and_then(|p| if end { p.end_child() } else { p.start_child() })
            .map_or(0, |w| w.width());
        widths[usize::from(end)] = width;
        let (ok, seen) = (text.as_str() == diff::normalise(want), seen(&view));
        if !ok || (seen == 0 && !want.is_empty()) {
            bad.push(if end { "new" } else { "old" });
        }
        format!(
            "{}:lines={},ok={ok},seen={seen},width={width}",
            if end { "new" } else { "old" },
            buffer.line_count(),
        )
    };
    let (old, new) = (side(false), side(true));
    if (widths[0] - widths[1]).abs() > 2 {
        bad.push("split");
    }
    format!(
        "front={front:?} {old} {new} {} bad={bad:?}",
        bench_compare_line(&compare),
    )
}

/// How many lines with text in them show some of it inside `view`'s visible rectangle: from the
/// top of the first character to the bottom of the last, padding left out.
fn seen(view: &gtk::TextView) -> usize {
    let rect = view.visible_rect();
    let bottom = rect.y() + rect.height();
    let (mut at, _) = view.line_at_y(rect.y());
    let mut n = 0;
    loop {
        let top = view.iter_location(&at).y();
        if top >= bottom {
            break;
        }
        let mut end = at;
        end.forward_to_line_end();
        let last = view.iter_location(&end);
        if !at.ends_line() && view.line_yrange(&at).1 > 0 && last.y() + last.height() > rect.y() {
            n += 1;
        }
        if !at.forward_line() {
            break;
        }
    }
    n
}

/// A Changes row clicked while the file's own tab is behind the disk (`=stale:<rel>`): the note
/// is opened as the window comes up and changed on disk while the first index is still walking,
/// which takes the change in before the watcher's news of it, so the news reads as no change and
/// the tab is never told. Its row is clicked as soon as the Git pane lists it, and the drill
/// prints whether the tab had the disk's text then and what the click opened 1.5 s on, as
/// `=clicks` does. Point it at a clean repository large enough to take seconds to index: `make
/// vault` into a scratch folder, committed.
pub(in crate::bench) fn bench_compare_stale(app: &Rc<App>, rel: &str) {
    app.show_pane("git");
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        let mut tab = None;
        for _ in 0..100 {
            wait(50).await;
            tab = app.open_tabs().into_iter().find(|tab| tab.rel() == rel);
            if tab.is_some() {
                break;
            }
        }
        let Some(tab) = tab else {
            println!("bench compare_stale no_tab");
            return bench_quit(&app);
        };
        let text = |tab: &Tab| {
            let (s, e) = tab.buffer.bounds();
            tab.buffer.text(&s, &e, true).to_string()
        };
        let index = text(&tab);
        let work = format!("{index}changed behind the tab\n");
        let _ = std::fs::write(app.root().join(&rel), &work);
        let started = std::time::Instant::now();
        let Some(panel) = app.git.get().cloned() else {
            println!("bench compare_stale no_git");
            return bench_quit(&app);
        };
        let mut listed = false;
        for _ in 0..600 {
            wait(50).await;
            if panel.changes_rows() > 0 {
                listed = true;
                break;
            }
        }
        println!(
            "bench compare_stale listed={listed} after={}ms reconciled={} tab_current={}",
            started.elapsed().as_millis(),
            app.reconciled.get(),
            text(&tab) == work
        );
        let report = |what: &str| {
            println!(
                "bench compare_stale {what} t={} reconciled={} tab_current={} {}",
                started.elapsed().as_millis(),
                app.reconciled.get(),
                text(&tab) == work,
                columns(&app, &rel, &(index.clone(), work.clone()))
            );
        };
        println!("bench compare_stale row={:?}", panel.activate_change(&rel));
        wait(1500).await;
        report("clicked");
        bench_quit(&app);
    });
}
