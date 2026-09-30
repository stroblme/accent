//! Drills over comparisons: a note against its disk copy, two blobs, and the working tree.

use super::diagnostics::diagnostic;
use super::*;
use accent_api::{Fold, Severity};

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
pub(super) fn bench_compare(app: &Rc<App>, rel: &str) {
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
pub(super) fn bench_compare_row(app: &Rc<App>, rel: &str) {
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

/// What the toast over the window reads, whatever it says: [`bench_said`] looks for a failure.
pub(super) fn bench_toast(app: &Rc<App>) -> Option<String> {
    let toast = find_widget(app.window.upcast_ref(), &|w| {
        w.type_().name() == "AdwToastWidget"
    })?;
    let label = find_widget(&toast, &|w| w.is::<gtk::Label>())?;
    Some(label.downcast::<gtk::Label>().ok()?.label().to_string())
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
pub(super) fn bench_compare_diag(app: &Rc<App>, rel: &str) {
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
pub(super) fn bench_compare_gutter(app: &Rc<App>, rel: &str) {
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
            // A change of the range, not a notify: GTK re-sets an unchanged one at the bottom.
            let (tick, upper) = (count(&moves), Cell::new(adj.upper()));
            let mut ids = vec![(
                adj.clone().upcast::<glib::Object>(),
                adj.connect_upper_notify(move |a| {
                    if upper.replace(a.upper()) != a.upper() {
                        tick();
                    }
                }),
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

/// Whether the paragraph at `at` carries alignment padding above or below it, which is what GTK
/// lays the line out with and what a character typed at the line's start must not take away.
fn is_padded(tab: &Rc<Tab>, at: &gtk::TextIter) -> bool {
    let (above, below) = (tab.view.pixels_above_lines(), tab.view.pixels_below_lines());
    at.tags().iter().any(|t| {
        (t.is_pixels_above_lines_set() && t.pixels_above_lines() > above)
            || (t.is_pixels_below_lines_set() && t.pixels_below_lines() > below)
    })
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
pub(super) fn bench_compare_conflict(app: &Rc<App>, rel: &str) {
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
pub(super) fn bench_compare_left(app: &Rc<App>, rel: &str) {
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
pub(super) fn bench_compare_page(app: &Rc<App>, rel: &str) {
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
/// again in a tab of two blobs. Opening the first run used to scroll the view by twice the run's height, which
/// both views took back from the scroll they share. Writes the note, so point it at a scratch
/// vault.
pub(super) fn bench_compare_gap(app: &Rc<App>, rel: &str) {
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
        match pane_view(compare.widget(), true) {
            Some(view) => println!(
                "bench compare_gap blobs {}",
                bench_gap(compare, &view, None).await
            ),
            None => println!("bench compare_gap blobs none"),
        }
        bench_quit(&app);
    });
}

/// Scroll the button of the hidden run `pick` on `view` (the middle one for `None`) halfway down
/// the view, as far as the view goes, press it as a pointer does, the focus and then `clicked`, and
/// say where the scroll and the lines above and under the button were before and after, and how far
/// the scroll strayed meanwhile.
async fn bench_gap(compare: &diff::Compare, view: &gtk::TextView, pick: Option<usize>) -> String {
    let adj = compare.vadjustment();
    let mut buttons = Vec::new();
    let mut stack = vec![view.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = stack.pop() {
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            if let Ok(button) = c.clone().downcast::<gtk::Button>()
                && button.is_visible()
                && button.label().is_some_and(|l| l.starts_with('⋯'))
            {
                buttons.push(button);
            }
            stack.push(c);
        }
    }
    let top = |button: &gtk::Button| {
        let at = button.compute_point(view, &gtk::graphene::Point::zero());
        at.map_or(0, |p| p.y() as i32) + view.visible_rect().y()
    };
    buttons.sort_by_key(top);
    let Some(button) = buttons.get(pick.unwrap_or(buttons.len() / 2)).cloned() else {
        return format!("buttons={}", buttons.len());
    };
    let seen = view.visible_rect();
    adj.set_value(adj.value() + f64::from(top(&button) - seen.y()) - adj.page_size() / 2.0);
    glib::timeout_future(Duration::from_millis(500)).await;
    // Kept as marks, since the click lays both buffers again. The button sits in the padding of
    // the line under the run; the line above is the visible one before that, if there is one.
    let buffer = view.buffer();
    let (below, _) = view.line_at_y(top(&button));
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

/// A note with its first section folded, compared with its disk copy, which differs only at the
/// end: the run the comparison collapses reaches over the fold's end. It asks for the iter at every
/// pixel row of the editor, as GtkSourceView asks at the top and bottom of the screen on every
/// frame, and prints whether the fold was open meanwhile and is shut again once the comparison
/// has gone. With the fold left shut, the line where it ended was laid out as a blank row, and an
/// iter asked in that row's pixels-below-lines aborted the process ("Byte index … is off the end of
/// the line"). Writes the note, so point it at a scratch vault.
pub(super) fn bench_compare_folds(app: &Rc<App>, rel: &str) {
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

/// Stage Selected Lines and Unstage Selected Lines, end to end. It makes a repository in the
/// vault root and commits the note as twelve lines, so point it at a throwaway vault. The tab then
/// rewrites line 3 and adds a line under line 9, and the working tree is compared with the index;
/// line 3 is selected in the editor and staged, then selected in the Index pane of the staged
/// comparison and unstaged. It prints the entry each pane's menu offers and what the index holds
/// after each step, then leaves line 3 selected in the working-tree comparison for eight seconds,
/// for `build-aux/xtest.py` to open the menu on with a secondary click (`hold` says when).
pub(super) fn bench_compare_lines(app: &Rc<App>, rel: &str) {
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
            .map(|v| v.and_then(label)),
            bench_compare_line(&compare)
        );
        select_line(&tab.buffer, 2);
        let _ = tab.view.activate_action("diff.selection", None);
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
            views.clone().map(|v| v.and_then(label)),
            bench_compare_line(staged.comparison())
        );
        if let [_, Some(view)] = views {
            select_line(&view.buffer(), 2);
            let _ = view.activate_action("diff.selection", None);
        }
        wait(1500).await;
        println!(
            "bench compare_lines unstaged index={:?} {}",
            git(&["show", &index]),
            bench_compare_line(staged.comparison())
        );

        app.reveal_page(&tab.page);
        select_line(&tab.buffer, 2);
        println!("bench compare_lines hold");
        wait(8000).await;
        bench_quit(&app);
    });
}

/// The view of one pane of a comparison: the left one, or the right with `end`.
pub(super) fn pane_view(paned: &gtk::Widget, end: bool) -> Option<gtk::TextView> {
    let paned = paned.downcast_ref::<gtk::Paned>()?;
    let pane = match end {
        true => paned.end_child(),
        false => paned.start_child(),
    }?;
    find_widget(&pane, &|w| w.is::<gtk::TextView>())?
        .downcast()
        .ok()
}

/// The first entry of the context menu a view adds to GTK's own, which is in a section.
fn label(view: gtk::TextView) -> Option<String> {
    view.extra_menu()?
        .item_link(0, "section")?
        .item_attribute_value(0, "label", None)?
        .get::<String>()
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

fn bench_compare_line(compare: &diff::Compare) -> String {
    let (rows, hunks, hidden, buttons) = compare.counts();
    format!(
        "rows={rows} hunks={hunks} hidden={hidden} buttons={buttons} misaligned={} skew={}",
        compare.misaligned(),
        compare.skew()
    )
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

pub(super) fn bench_compare_pads(app: &Rc<App>, rel: &str) {
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

/// A Changes row clicked right after another repository is picked (`=pick:<rel>`). Point it at a
/// vault whose root repository has `<rel>` changed and a second repository beside it: the drill
/// picks the second one in the chooser and activates `<rel>`'s row at once, before the refresh
/// the pick asks for has landed, and prints whether that row was still listed and, for whatever
/// comparison opened, both sides' line counts. The list used to go on showing the root's rows
/// until then, and the row compared its path in the repository just picked, where git has no
/// such file: an empty Index side and the whole file drawn as added, deletions and all. Then it
/// picks the root again, lets the refresh land, and prints the same for a click on the row.
pub(super) fn bench_compare_pick(app: &Rc<App>, rel: &str) {
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
