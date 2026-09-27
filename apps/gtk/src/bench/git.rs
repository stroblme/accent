//! Drills over the Git pane: its lists, the fetch on opening, and Stage clicks.

use super::*;

/// Show the Git pane, print how many rows its two lists hold, flip the changes list between the
/// tree and the flat view, and activate the history's last row — the Load More row — printing the
/// counts again. The headless image has no pointer, so this is the only way "Load More is the end
/// of the list and paging it in works" and "the tree adds a row per folder" are provable.
pub(super) fn bench_git(app: &Rc<App>) {
    app.show_pane("git");
    let app = app.clone();
    // Long enough for the debounced refresh and its `git status` and `git log` to land.
    glib::timeout_add_local_once(Duration::from_millis(2500), move || {
        let Some(git) = app.git.get() else {
            return bench_quit(&app);
        };
        // Again: the session restore runs in the post-present idle and deliberately puts Files
        // back, so the pane this hook is about is not the one on screen by the time it reads it.
        app.show_pane("git");
        println!(
            "bench git_changes tree={} {}",
            git.tree(),
            git.changes_rows()
        );
        git.set_tree(!git.tree());
        println!(
            "bench git_changes tree={} {}",
            git.tree(),
            git.changes_rows()
        );
        println!("bench git_rows {}", git.log_rows());
        // What the fetch on opening the vault bought: the branch readout the status bar shows —
        // the dot in front of it means uncommitted work — and how many history rows are drawn as
        // not pulled yet, which only a fetch can have found.
        println!(
            "bench git_branch {}",
            git.branch_label(None).unwrap_or_default()
        );
        // `false` on a cold vault: the pane answered while the walk was still going.
        println!("bench git_indexed {}", app.reconciled.get());
        let (local, remote) = git.branch_counts();
        println!("bench git_branches local={local} remote={remote}");
        println!("bench git_not_pulled {}", git.not_pulled_rows());
        println!("bench git_sync {}", git.sync_hint().unwrap_or_default());
        let (live, tip) = git.commit_hint();
        println!("bench git_commit live={live} {}", tip.unwrap_or_default());
        git.activate_last_log_row();
        let app = app.clone();
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            if let Some(git) = app.git.get() {
                println!("bench git_rows {}", git.log_rows());
            }
            bench_git_stage(&app);
        });
    });
}

/// Whether the Git pane is on the switcher, before and after a `git init` in the vault root.
///
/// A vault with no repository has no Git pane (DESIGN.md, Layout map), so the page that used to
/// say so was never reachable. What replaced it has nothing to click: `git init` writes inside
/// `.git`, the watcher reports that write like any other, and a vault with no repository answers
/// one by going looking for repositories. Point it at a throwaway vault that is not a repository;
/// it makes one, so never run it anywhere that matters.
pub(super) fn bench_git_init(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        // The first refresh has to have landed, or the pane would be hidden for not having been
        // asked yet rather than for having no repository.
        glib::timeout_future(Duration::from_millis(2500)).await;
        println!("bench git_pane {}", has_git_pane(&app));
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(app.root())
            .status();
        println!("bench git_init {}", status.is_ok_and(|s| s.success()));
        // The watcher's own debounce, the pane's, and a discovery that runs git per directory.
        glib::timeout_future(Duration::from_millis(4000)).await;
        println!("bench git_pane {}", has_git_pane(&app));
        bench_quit(&app);
    });
}

/// A commit row of the history clicked the moment another repository is picked, before the
/// refresh that reads the new one's has landed. Point it at a vault holding two repositories with
/// a commit each. The row used to be the old repository's, asked of the new one, which toasted
/// `Cannot list the commit's files: fatal: bad object …`; now the history is empty until then.
pub(super) fn bench_git_switch(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        wait_for(
            || app.git.get().is_some_and(|git| git.log_rows() > 0),
            30000,
        )
        .await;
        let Some(git) = app.git.get().cloned() else {
            return bench_quit(&app);
        };
        println!("bench git_switch before rows={}", git.log_rows());
        let said = app.toasted.get();
        git.select_repo(1);
        let rows = git.log_rows();
        git.activate_log_row(0);
        glib::timeout_future(Duration::from_millis(1500)).await;
        println!(
            "bench git_switch clicked rows={rows} toasts={} said={:?} after={}",
            app.toasted.get() - said,
            bench_said(&app),
            git.log_rows()
        );
        bench_quit(&app);
    });
}

/// Whether the sidebar is showing a Git pane at all, which is the whole readout above: the pane
/// appearing is a switcher icon, and the headless image has no pointer to find it with.
fn has_git_pane(app: &Rc<App>) -> bool {
    app.sidebar.get().is_some_and(|s| s.has_pane("git"))
}

/// The changes list's splices, printed as they happen, across a refresh that changes nothing and
/// two Stage clicks, and whether a row below the staged one kept its widget. A row that is spliced
/// out from under a press loses its release, so this is the headless half of "rapid Stage clicks
/// all land". Point it at a repository with `b.md` and `c.md` modified and `new.md` untracked at
/// its root, and nothing staged; elsewhere it prints the splices and the clicks it could not make.
fn bench_git_stage(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let Some(git) = app.git.get() else {
            return bench_quit(&app);
        };
        git.set_tree(true);
        let list = git.divider().start_child();
        let view = list
            .as_ref()
            .and_then(|w| find_widget(w, &|w| w.is::<gtk::ListView>()))
            .and_downcast::<gtk::ListView>();
        if let Some(model) = view.as_ref().and_then(|v| v.model()) {
            model.connect_items_changed(|_, at, removed, added| {
                println!("bench git_splice at={at} removed={removed} added={added}");
            });
        }
        let row = |path: &str| list.as_ref().and_then(|list| change_row(list, path));
        let click = |path: &str, tooltip: &str| {
            let button = row(path).and_then(|r| row_button(&r, tooltip));
            println!("bench git_click {path} {tooltip} {}", button.is_some());
            if let Some(button) = button {
                button.emit_clicked();
            }
        };
        if let Some(list) = list.as_ref() {
            bench_git_hover(list, "src").await;
        }
        bench_git_commit(&app, git.divider().end_child()).await;
        // The command's own refresh and the one its `.git` write schedules both land in this.
        let settle = || glib::timeout_future(Duration::from_millis(1500));

        println!("bench git_step refresh_unchanged");
        git.schedule_refresh(crate::git::Depth::Everything);
        settle().await;

        for (staged, below) in [("b.md", "c.md"), ("c.md", "new.md")] {
            println!("bench git_step stage {staged}");
            let before = row(below);
            click(staged, "Stage");
            settle().await;
            let kept = before.is_some() && before == row(below);
            println!("bench git_kept {below} {kept}");
        }
        println!("bench git_changes_rows {}", git.changes_rows());
        if let Some(view) = &view {
            bench_git_headers(view);
        }

        // A folder's own buttons: all of `src` into the index and out again, then its Discard,
        // whose question is printed and answered — and the permanent delete's too, where the
        // scratch directory has no trash.
        for tooltip in ["Stage", "Unstage"] {
            println!("bench git_step folder {tooltip}");
            click("src", tooltip);
            settle().await;
            println!("bench git_changes_rows {}", git.changes_rows());
        }
        println!("bench git_step folder Discard");
        click("src", "Discard");
        // Discard, and the Delete Permanently the untracked half may raise behind it: both are
        // `dialogs::confirm` questions, so both answer to the one id.
        for _ in 0..2 {
            glib::timeout_future(Duration::from_millis(500)).await;
            let Some(dialog) = app
                .window
                .visible_dialog()
                .and_downcast::<adw::AlertDialog>()
            else {
                continue;
            };
            let (heading, body) = (dialog.heading().unwrap_or_default(), dialog.body());
            println!("bench git_dialog {heading:?} {body:?}");
            dialog.emit_by_name::<()>("response", &[&crate::dialogs::CONFIRM]);
            dialog.close();
            settle().await;
        }
        println!("bench git_changes_rows {}", git.changes_rows());

        // The Changes header's Discard All: a staged file changed again and a file made that git
        // does not track, its one question printed and answered, and what is left of either.
        let (changed, made) = (app.root().join("b.md"), app.root().join("all-new.md"));
        let _ = std::fs::write(&changed, "discarded\n");
        let _ = std::fs::write(&made, "untracked\n");
        git.schedule_refresh(crate::git::Depth::Status);
        settle().await;
        let header = list.as_ref().and_then(|list| {
            find_widget(list, &|w| {
                w.is_mapped()
                    && w.downcast_ref::<gtk::Stack>().is_some_and(|s| {
                        s.visible_child_name().as_deref() == Some("header")
                            && s.visible_child()
                                .and_then(|h| h.first_child())
                                .and_downcast::<gtk::Label>()
                                .is_some_and(|l| l.text() == "Changes")
                    })
            })
        });
        let button = header.and_then(|h| row_button(&h, "Discard All"));
        println!(
            "bench git_step discard_all button={} rows={}",
            button.is_some(),
            git.changes_rows()
        );
        if let Some(button) = button {
            button.emit_clicked();
        }
        for _ in 0..2 {
            glib::timeout_future(Duration::from_millis(500)).await;
            let Some(dialog) = app
                .window
                .visible_dialog()
                .and_downcast::<adw::AlertDialog>()
            else {
                continue;
            };
            let (heading, body) = (dialog.heading().unwrap_or_default(), dialog.body());
            println!("bench git_dialog {heading:?} {body:?}");
            dialog.emit_by_name::<()>("response", &[&crate::dialogs::CONFIRM]);
            dialog.close();
            settle().await;
        }
        println!(
            "bench git_discard_all rows={} reverted={} untracked_gone={}",
            git.changes_rows(),
            std::fs::read_to_string(&changed).is_ok_and(|t| t != "discarded\n"),
            !made.exists()
        );
        bench_quit(&app);
    });
}

/// The section headers on screen: their title, their bulk button's icon and tooltip, the Discard
/// All beside it where the header offers one, and whether their row is `.activatable`, the class
/// Adwaita's hover and press highlight is written for. The file and folder rows are counted beside
/// them, and every one of those is to keep it.
fn bench_git_headers(view: &gtk::ListView) {
    let (mut rows, mut lit) = (0, 0);
    let mut row = view.first_child();
    while let Some(r) = row {
        row = r.next_sibling();
        // Only a mapped row: the list keeps the widgets it has no item for, with their old names.
        let stack = r.first_child().and_downcast::<gtk::Stack>();
        let Some(stack) = stack.filter(|_| r.is_mapped()) else {
            continue;
        };
        let activatable = r.has_css_class("activatable");
        if stack.visible_child_name().as_deref() != Some("header") {
            rows += 1;
            lit += usize::from(activatable);
            continue;
        }
        let header = stack.visible_child();
        let title = header.as_ref().and_then(|h| h.first_child());
        let button = title
            .as_ref()
            .and_then(|t| t.next_sibling())
            .and_downcast::<gtk::Button>();
        let discard = header
            .as_ref()
            .and_then(|h| h.last_child())
            .and_downcast::<gtk::Button>()
            .filter(|b| b.is_visible())
            .map(|b| (b.icon_name(), b.tooltip_text()));
        println!(
            "bench git_header {:?} activatable={activatable} icon={:?} tip={:?} discard={discard:?}",
            title.and_downcast::<gtk::Label>().map(|l| l.text()),
            button.as_ref().and_then(|b| b.icon_name()),
            button.as_ref().and_then(|b| b.tooltip_text()),
        );
    }
    println!("bench git_header_rows activatable={lit}/{rows}");
}

/// What a row's name has room for either side of the pointer arriving on it.
///
/// The buttons sit in a revealer, so while they are away they measure nothing and the label has
/// the whole width of the pane; the truncation starts only where they really appear. PRELIGHT is
/// set by hand — Xvfb has no pointer — which is the flag GTK puts on the row it is over and the
/// one `reveal_on_hover` watches.
async fn bench_git_hover(list: &gtk::Widget, path: &str) {
    let stack = change_row(list, path).and_downcast::<gtk::Stack>();
    let shown = stack.as_ref().and_then(|stack| stack.visible_child());
    let label = shown
        .as_ref()
        .and_then(|shown| {
            find_widget(shown, &|w| {
                w.downcast_ref::<gtk::Label>()
                    .is_some_and(WidgetExt::hexpands)
            })
        })
        .and_downcast::<gtk::Label>();
    let buttons = shown
        .as_ref()
        .and_then(|shown| shown.last_child())
        .and_downcast::<gtk::Revealer>();
    let (Some(label), Some(buttons), Some(row)) =
        (label, buttons, stack.and_then(|stack| stack.parent()))
    else {
        return println!("bench git_hover {path} none");
    };
    for on in [false, true, false] {
        match on {
            true => row.set_state_flags(gtk::StateFlags::PRELIGHT, false),
            false => row.unset_state_flags(gtk::StateFlags::PRELIGHT),
        }
        // A tenth of a second into the slide, whether the buttons are still painted: they leave
        // at full opacity as they came, rather than vanishing before the gap closes.
        glib::timeout_future(Duration::from_millis(100)).await;
        let sliding = buttons.child().is_some_and(|b| painted(&b));
        // Past the reveal's own slide, which is what the label's width waits on.
        glib::timeout_future(Duration::from_millis(500)).await;
        println!(
            "bench git_hover {path} pointer={on} name={} buttons={} revealed={} \
             painted_mid_slide={sliding}",
            label.width(),
            buttons.width(),
            buttons.reveals_child()
        );
    }
}

/// Whether `widget` draws anything. One at CSS opacity 0, or not drawn at all, has no render
/// node, so its paintable is empty.
fn painted(widget: &gtk::Widget) -> bool {
    let snapshot = gtk::Snapshot::new();
    let (w, h) = (f64::from(widget.width()), f64::from(widget.height()));
    gtk::WidgetPaintable::new(Some(widget)).snapshot(&snapshot, w, h);
    snapshot.to_node().is_some()
}

/// A commit row's own two buttons, which used to be a secondary-click menu.
///
/// The same surface a changed file's actions have: what the row offers, held in a revealer that
/// measures nothing until the pointer or the keyboard is on the row. PRELIGHT is set by hand for
/// the reason [`bench_git_hover`] gives, and Copy Commit ID is then pressed, which is one toast.
async fn bench_git_commit(app: &Rc<App>, list: Option<gtk::Widget>) {
    let row = list.as_ref().and_then(|list| {
        find_widget(list, &|w| {
            w.downcast_ref::<gtk::Stack>()
                .is_some_and(|s| s.visible_child_name().as_deref() == Some("commit"))
        })
    });
    let revealer = row
        .as_ref()
        .and_then(|row| row.downcast_ref::<gtk::Stack>()?.visible_child())
        .and_then(|shown| shown.last_child())
        .and_downcast::<gtk::Revealer>();
    let (Some(row), Some(revealer)) = (row, revealer) else {
        return println!("bench git_commit_row none");
    };
    let Some(list_row) = row.parent() else {
        return println!("bench git_commit_row unparented");
    };
    let mut tooltips = Vec::new();
    let mut child = revealer.child().and_then(|box_| box_.first_child());
    while let Some(button) = child {
        tooltips.push(button.tooltip_text().map(|t| t.to_string()));
        child = button.next_sibling();
    }
    println!("bench git_commit_row buttons={tooltips:?}");
    for on in [false, true] {
        match on {
            true => list_row.set_state_flags(gtk::StateFlags::PRELIGHT, false),
            false => list_row.unset_state_flags(gtk::StateFlags::PRELIGHT),
        }
        glib::timeout_future(Duration::from_millis(600)).await;
        println!(
            "bench git_commit_row pointer={on} width={} revealed={}",
            revealer.width(),
            revealer.reveals_child()
        );
    }
    let said = app.toasted.get();
    match row_button(&row, "Copy Commit ID") {
        Some(button) => button.emit_clicked(),
        None => println!("bench git_commit_row no_copy_button"),
    }
    glib::timeout_future(Duration::from_millis(300)).await;
    println!(
        "bench git_commit_row copied toasts={}",
        app.toasted.get() - said
    );
}

/// Hold a real press on the Stage button of `path`'s row while the list changes under it. Prints
/// where the button is on screen (`bench git_press x y`, window coordinates) and stays up for ten
/// seconds, so that a script can press there through XTEST, change the repository and release; the
/// repository's `git status` afterwards says whether the click landed. `emit_clicked` cannot show
/// this, having no press and no release to take apart.
pub(super) fn bench_git_press(app: &Rc<App>, path: &str) {
    app.show_pane("git");
    let (app, path) = (app.clone(), path.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(2500)).await;
        let Some(git) = app.git.get() else {
            return bench_quit(&app);
        };
        app.show_pane("git");
        git.set_tree(true);
        glib::timeout_future(Duration::from_millis(300)).await;
        let list = git.divider().start_child();
        let button = list
            .as_ref()
            .and_then(|list| change_row(list, &path))
            .and_then(|row| row_button(&row, "Stage"));
        let at = button.and_then(|b| {
            let middle = graphene::Point::new(b.width() as f32 / 2.0, b.height() as f32 / 2.0);
            b.compute_point(&app.window, &middle)
        });
        match at {
            Some(at) => println!("bench git_press {} {}", at.x() as i32, at.y() as i32),
            None => println!("bench git_press none"),
        }
        glib::timeout_future(Duration::from_secs(10)).await;
        bench_quit(&app);
    });
}

/// Whether a real click leaves a row's buttons out once the pointer has gone, against what the
/// keyboard does, driven through XTEST (`build-aux/xtest.py`, spawned per step): the first commit
/// row hovered, clicked open and shut with the pointer moved off each time, the keyboard walked
/// off it and back, left there past GTK's three seconds of visible focus, Tabbed into its first
/// button and back, its Copy Commit ID clicked and the row clicked once more; then the Changes
/// list's `src` folder pressed, walked back to and clicked. Point it at the repository the default
/// drill wants. Every pointer step but the hover and a press still held ends `commit=false
/// folder=false`, and every keyboard step on a row prints that row's buttons out.
pub(super) fn bench_git_focus(app: &Rc<App>) {
    app.show_pane("git");
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(2500)).await;
        let Some(git) = app.git.get() else {
            return bench_quit(&app);
        };
        app.show_pane("git");
        git.set_tree(true);
        glib::timeout_future(Duration::from_millis(300)).await;
        // Looked for afresh each time: a folder that collapses rebuilds the rows it is drawn in.
        let (history, changes) = (git.divider().end_child(), git.divider().start_child());
        // Only a mapped row: the list keeps the widgets it has no item for, with their old names.
        let find = |list: &Option<gtk::Widget>, name: &str, tooltip: Option<&str>| {
            list.as_ref().and_then(|list| {
                find_widget(list, &|w| {
                    w.is_mapped()
                        && w.downcast_ref::<gtk::Stack>()
                            .is_some_and(|s| s.visible_child_name().as_deref() == Some(name))
                        && tooltip.is_none_or(|t| w.tooltip_text().as_deref() == Some(t))
                })
            })
        };
        let commit = || find(&history, "commit", None);
        let folder = || find(&changes, "folder", Some("src"));
        if commit().is_none() || folder().is_none() {
            println!("bench git_focus none");
            return bench_quit(&app);
        }
        // Screen coordinates, left of the row's name: no window manager places the window.
        let (dx, dy) = app.window.surface_transform();
        let spot = |w: Option<gtk::Widget>| {
            let at = w.and_then(|w| {
                let at = graphene::Point::new(20.0, w.height() as f32 / 2.0);
                w.compute_point(&app.window, &at)
            });
            at.map(|p| format!("{} {}", p.x() as f64 + dx, p.y() as f64 + dy))
                .unwrap_or_default()
        };
        let off = format!("{} {}", app.window.width() - 40, app.window.height() / 2);
        let revealed = |stack: Option<gtk::Widget>| {
            stack
                .and_downcast::<gtk::Stack>()
                .and_then(|s| s.visible_child()?.last_child())
                .and_downcast::<gtk::Revealer>()
                .is_some_and(|r| r.reveals_child())
        };
        let say = |step: &str| {
            let focus = gtk::prelude::GtkWindowExt::focus(&app.window);
            let within = |row: Option<gtk::Widget>| {
                let row = row.and_then(|stack| stack.parent());
                focus
                    .as_ref()
                    .zip(row)
                    .is_some_and(|(f, row)| f == &row || f.is_ancestor(&row))
            };
            let on = match (within(commit()), within(folder())) {
                (true, _) => "commit",
                (_, true) => "folder",
                _ => "other",
            };
            // The buttons of whichever row holds the keyboard, which after a rebuild can be one
            // bound to another item since.
            let row = focus.as_ref().and_then(|f| {
                std::iter::successors(Some(f.clone()), |w| w.parent())
                    .find(|w| w.first_child().is_some_and(|c| c.is::<gtk::Stack>()))
            });
            println!(
                "bench git_focus {step} commit={} folder={} focus={} on={on} focused_row={} \
                 visible={} toasts={} history_rows={}",
                revealed(commit()),
                revealed(folder()),
                focus.as_ref().map_or("none", |f| f.type_().name()),
                revealed(row.and_then(|r| r.first_child())),
                app.window.gets_focus_visible(),
                app.toasted.get(),
                git.log_rows()
            );
        };
        let click = |w| format!("move {}; down; up; sleep 0.3; move {off}", spot(w));
        xtest(&format!("move {}; focus", spot(commit()))).await;
        say("commit_hover");
        xtest(&click(commit())).await;
        say("commit_click_open");
        xtest(&click(commit())).await;
        say("commit_click_shut");
        xtest("key Down").await;
        say("key_down");
        xtest("key Up").await;
        say("key_up");
        glib::timeout_future(Duration::from_millis(3500)).await;
        say("key_idle");
        xtest("key Tab").await;
        say("key_tab");
        xtest("key shift+Tab").await;
        say("key_back");
        // The pointer pressing one of those buttons while the keyboard is on the row.
        let copy = commit().and_then(|row| row_button(&row, "Copy Commit ID"));
        xtest(&click(copy.map(|b| b.upcast()))).await;
        say("commit_button_click");
        xtest(&click(commit())).await;
        say("commit_click_after_keys");
        // A press let go of off the row, which focuses it without folding it until the release;
        // the release redraws the list, which hands the focus to its header.
        xtest(&format!("move {}; down", spot(folder()))).await;
        say("folder_down");
        xtest(&format!("move {off}; up")).await;
        say("folder_press");
        xtest("key Down").await;
        say("folder_key");
        xtest(&click(folder())).await;
        say("folder_click");
        bench_quit(&app);
    });
}

/// Run `build-aux/xtest.py` on this display, then wait out the buttons' 250 ms slide.
async fn xtest(steps: &str) {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../../build-aux/xtest.py");
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let argv = ["python3", script, &display, steps].map(std::ffi::OsStr::new);
    if let Ok(run) = gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE) {
        let _ = run.wait_future().await;
    }
    glib::timeout_future(Duration::from_millis(600)).await;
}

/// The changes list's row for `path`, a file or a folder. A header row keeps whatever tooltip its
/// widget last had, so the layout it shows is asked as well.
fn change_row(list: &gtk::Widget, path: &str) -> Option<gtk::Widget> {
    find_widget(list, &|w| {
        w.downcast_ref::<gtk::Stack>().is_some_and(|s| {
            matches!(s.visible_child_name().as_deref(), Some("entry" | "folder"))
                && s.tooltip_text().as_deref() == Some(path)
        })
    })
}

/// The button of `row` that `tooltip` names, where the row shows it.
fn row_button(row: &gtk::Widget, tooltip: &str) -> Option<gtk::Button> {
    find_widget(row, &|w| {
        w.is::<gtk::Button>() && w.is_visible() && w.tooltip_text().as_deref() == Some(tooltip)
    })
    .and_downcast::<gtk::Button>()
}

/// Close the window while git runs, and print what the close did (DESIGN.md, States).
///
/// `pull` starts a Sync and closes during its pull, which asks, is answered Close When Finished
/// and waits for the whole sync; `push` closes during that sync's push and `fetch` during the
/// fetch on opening the vault, which are stopped and close at once. Point it at a scratch clone
/// whose origin is slow both ways — `remote.origin.uploadpack` set to `sleep 3; git-upload-pack`,
/// a `pre-receive` hook in the origin that sleeps 3 s — with a commit to push, and launch it from
/// outside the clone's parent directory: what it prints last is how many processes are still
/// working in there, which is to be none. On a remote vault the processes are the host's, so
/// `pull` and `push` go by the pane's own word for the half, and `fetch` is not for one.
pub(super) fn bench_git_close(app: &Rc<App>, phase: &str) {
    let Some(gtk_app) = app.window.application() else {
        return bench_quit(app);
    };
    let (app, phase) = (app.clone(), phase.to_string());
    glib::spawn_future_local(async move {
        // The printing goes on after the window has gone.
        let _hold = gtk_app.hold();
        // A remote vault's pane and its repository come with the host's answers, which on a slow
        // link take their time: waited for, not timed.
        wait_for(|| app.git.get().is_some_and(|git| git.has_repos()), REPOS).await;
        let Some(git) = app.git.get().cloned() else {
            return bench_quit(&app);
        };
        let root = app.root().canonicalize().unwrap_or_else(|_| app.root());
        let running = |what: &str| accent_api::git::running(&root, what);
        // Polled rather than a `destroy` handler: this drill holds the window, so a closed one is
        // hidden and kept, and hiding it notifies nothing.
        let open = || app.window.is_visible();
        wait_for(|| running("fetch"), 3000).await;
        if phase != "fetch" {
            wait_for(|| !running("fetch"), 10000).await;
            git.sync(None);
            match phase.as_str() {
                // The pane's flag goes up just before git starts, and a local push is waited for
                // until git has it, so what is stopped is a push already running.
                "push" => {
                    let remote = app.vault().is_some_and(|v| v.is_remote());
                    wait_for(|| git.pushing() && (remote || running("push")), 20000).await
                }
                _ => glib::timeout_future(Duration::from_millis(800)).await,
            }
        }
        let dir = root.parent().unwrap_or(&root).to_path_buf();
        println!(
            "bench git_close phase={phase} fetch={} push={} pushing={} busy={} working={}",
            running("fetch"),
            running("push"),
            git.pushing(),
            git.busy(),
            working_in(&dir)
        );
        let asked = Instant::now();
        app.window.close();
        println!("bench git_close_now open={}", open());
        glib::timeout_future(Duration::from_millis(300)).await;
        let dialog = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>()
            .filter(|_| open());
        println!(
            "bench git_close_dialog {:?}",
            dialog.as_ref().and_then(|d| d.heading())
        );
        if let Some(dialog) = dialog {
            dialog.emit_by_name::<()>("response", &[&"wait"]);
            dialog.close();
            glib::timeout_future(Duration::from_millis(1000)).await;
            println!("bench git_close_waiting open={}", open());
        }
        wait_for(|| !open(), 20000).await;
        let said = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>();
        println!(
            "bench git_close_closed {} after_ms={:.0} dialog={:?}",
            !open(),
            ms_since(asked),
            said.and_then(|d| d.heading())
        );
        let t = Instant::now();
        wait_for(|| working_in(&dir) == 0, 5000).await;
        println!(
            "bench git_close_left {} after_ms={:.0}",
            working_in(&dir),
            ms_since(t)
        );
        std::process::exit(0);
    });
}

/// A Sync asked for while the fetch on opening the vault still runs, which must wait for that
/// fetch rather than race it for the remote-tracking refs (git's `cannot lock ref`). Point it at
/// the slow scratch clone `close:` uses. It prints whether the fetch was running when the Sync was
/// asked, whether a `git pull` was seen before the fetch had ended, and what the Sync left.
pub(super) fn bench_git_sync_over_fetch(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        wait_for(|| app.git.get().is_some_and(|git| git.has_repos()), REPOS).await;
        let Some(git) = app.git.get().cloned() else {
            return bench_quit(&app);
        };
        let root = app.root().canonicalize().unwrap_or_else(|_| app.root());
        let fetching = || accent_api::git::running(&root, "fetch");
        wait_for(fetching, 3000).await;
        let asked_while = fetching();
        git.sync(None);
        let mut pulled_early = false;
        while fetching() {
            pulled_early |= pulling(&root);
            glib::timeout_future(Duration::from_millis(20)).await;
        }
        // The whole Sync, push included: `busy` leaves a push out, a close not waiting for one.
        wait_for(|| !git.busy() && !git.pushing(), 20000).await;
        glib::timeout_future(Duration::from_millis(300)).await;
        // A toast may be queued behind another, so what the Sync did is read off the repository:
        // nothing left to pull is a pull that went through, and a refusal of several lines is a
        // dialog.
        let behind = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-list", "--count", "HEAD..@{u}"])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
        let dialog = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>()
            .map(|d| (d.heading(), d.body()));
        println!(
            "bench git_sync_over_fetch asked_while_fetching={asked_while} \
             pulled_early={pulled_early} behind_after={behind:?} toasts={} dialog={dialog:?}",
            app.toasted.get()
        );
        bench_quit(&app);
    });
}

/// Whether a `git pull` is running in `dir`, read off `/proc` as [`working_in`] reads the
/// processes there: a pull is not one of the transfers `git::running` can name.
fn pulling(dir: &Path) -> bool {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let cmdline = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
            cmdline.split(|b| *b == 0).any(|arg| arg == b"pull")
                && std::fs::read_link(entry.path().join("cwd"))
                    .is_ok_and(|cwd| cwd.starts_with(dir))
        })
}

/// The banner a stopped rebase raises, and Continue pressed through it twice. Point it at a scratch
/// repository whose rebase stopped on the first of two commits that conflict on `f.md`, with that
/// conflict resolved and staged: the first press stops on the second commit's conflict with the
/// banner still up, the drill resolves and stages it as a user would, and the second press
/// finishes. Abort asks first, and a dialog takes no answer under Xvfb, so it is not pressed.
pub(super) fn bench_git_rebase(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        wait_for(|| app.git.get().is_some_and(|git| git.has_repos()), REPOS).await;
        let Some(git) = app.git.get().cloned() else {
            return bench_quit(&app);
        };
        // The first refresh, which is what raises the banner.
        glib::timeout_future(Duration::from_millis(2500)).await;
        let root = app.root();
        let say = |when: &str| {
            let ((banner, button), (live, _)) = (git.banner_hint(), git.commit_hint());
            let rows = git.changes_rows();
            println!(
                "bench git_rebase {when} banner={banner:?} button={button} live={live} rows={rows}"
            );
        };
        for press in ["first", "second"] {
            say(press);
            git.press_commit();
            wait_for(|| !git.busy(), 20000).await;
            // The refresh the command brings, and a toast's worth of the watcher's after it.
            glib::timeout_future(Duration::from_millis(1500)).await;
            if press == "first" {
                say("stopped");
                let _ = std::fs::write(root.join("f.md"), "both two\n");
                let _ = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(["add", "f.md"])
                    .status();
                glib::timeout_future(Duration::from_millis(2500)).await;
            }
        }
        say("done");
        bench_quit(&app);
    });
}

/// How long a drill waits for the pane to find a repository, in ms: a host on a slow link has
/// been seen to take past 30 s over its first index, and the wait ends as soon as it has.
const REPOS: u64 = 120_000;

async fn wait_for(done: impl Fn() -> bool, ms: u64) {
    let t = Instant::now();
    while !done() && t.elapsed() < Duration::from_millis(ms) {
        glib::timeout_future(Duration::from_millis(50)).await;
    }
}

/// How many processes other than this one are working in `dir` or below it: git, its hooks, and
/// what they started. A zombie has no working directory left to read, so it does not count.
fn working_in(dir: &Path) -> usize {
    let me = std::process::id().to_string();
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_str() != Some(me.as_str()))
        .filter_map(|entry| std::fs::read_link(entry.path().join("cwd")).ok())
        .filter(|cwd| cwd.starts_with(dir))
        .count()
}

/// Git's conflict markers in an editor tab, over a note a real `git merge` left with three blocks
/// or more (`=markers:<rel>`). It prints the conflict tags and any heading tag on each line of the
/// note, then where each block's row of buttons sits against its first line — the line's top, the
/// row's top and bottom, the text's top — and holds 6 s for a screenshot. Then Next Conflict twice
/// and Previous Conflict once from the top, printing the caret's line; Accept Current on the first
/// block by its button, Accept Incoming from the palette's command with the caret in the next, and
/// Accept Both on the next by its button, printing the text after each; three undos, printing
/// whether the text is back; the first block's `=======` deleted and put back; and the rows while
/// a comparison is up and once it has gone.
pub(super) fn bench_git_markers(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        wait(1500).await;
        let Some(tab) = app.tab_for(&rel) else {
            println!("bench git_markers no_tab");
            return bench_quit(&app);
        };
        let original = tab.text();
        for n in 0..tab.buffer.line_count() {
            let Some(at) = tab.buffer.iter_at_line(n) else {
                continue;
            };
            let tags: Vec<String> = at
                .tags()
                .iter()
                .filter_map(|tag| tag.name())
                .filter(|name| {
                    name.starts_with("conflict") || highlight::HEADING_TAGS.contains(&name.as_str())
                })
                .map(String::from)
                .collect();
            let line = original.lines().nth(n as usize).unwrap_or_default();
            println!("bench git_markers line={} {line:?} {tags:?}", n + 1);
        }
        let conflicts = tab.conflicts();
        let rows = |when: &str| {
            let laid = conflicts.laid();
            let in_band = laid
                .iter()
                .flatten()
                .all(|[line, top, bottom, text]| line <= top && bottom <= text);
            println!("bench git_markers {when} rows={laid:?} in_band={in_band}");
        };
        rows("opened");
        println!("bench git_markers hold");
        wait(6000).await;

        let caret_line = || tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line() + 1;
        tab.buffer.place_cursor(&tab.buffer.start_iter());
        for action in ["conflict-next", "conflict-next", "conflict-previous"] {
            let _ = WidgetExt::activate_action(&app.window, &format!("win.{action}"), None);
            println!("bench git_markers {action} caret_line={}", caret_line());
        }

        let press = |label: &str| {
            let button = conflicts.row(0).and_then(|row| {
                find_widget(&row, &|w| {
                    w.downcast_ref::<gtk::Button>()
                        .is_some_and(|b| b.label().as_deref() == Some(label))
                })
            });
            if let Some(button) = button.and_downcast::<gtk::Button>() {
                button.emit_clicked();
            }
        };
        let say = |what: &str| println!("bench git_markers {what} text={:?}", tab.text());
        press("Accept Current");
        wait(300).await;
        say("current");
        let text = tab.text();
        if let Some(block) = accent_core::conflict::blocks(&text).first() {
            let at = text[..block.ours.start].chars().count() as i32;
            tab.buffer.place_cursor(&tab.buffer.iter_at_offset(at));
        }
        let _ = WidgetExt::activate_action(&app.window, "win.conflict-incoming", None);
        wait(300).await;
        say("incoming");
        press("Accept Both");
        wait(300).await;
        say("both");
        rows("resolved");
        for _ in 0..3 {
            tab.buffer.undo();
        }
        wait(300).await;
        println!("bench git_markers undone back={}", tab.text() == original);
        rows("undone");

        let text = tab.text();
        if let Some(block) = accent_core::conflict::blocks(&text).first() {
            let split = &block.markers()[1];
            let chars = |byte: usize| text[..byte].chars().count() as i32;
            let (mut from, mut to) = (
                tab.buffer.iter_at_offset(chars(split.start)),
                tab.buffer.iter_at_offset(chars(split.end)),
            );
            tab.buffer.delete(&mut from, &mut to);
        }
        wait(300).await;
        rows("split_deleted");
        tab.buffer.undo();
        wait(300).await;
        rows("split_back");

        tab.compare(
            "Mine",
            ("Disk", &original),
            diff::Side::New,
            false,
            None,
            "bench",
        );
        wait(500).await;
        rows("comparing");
        tab.leave_compare();
        wait(500).await;
        rows("compared");
        bench_quit(&app);
    });
}

/// Create Branch… with names git would refuse typed in (`=branch`): for each, what the line under
/// the field says, whether it shows, and whether Create is live. Then `my new branch` is created
/// by answering the dialog, a second after it was typed in (for a screenshot), and the branch HEAD
/// ends up on is printed: `my-new-branch` is the claim.
pub(super) fn bench_git_branch(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        wait_for(|| app.git.get().is_some_and(|git| git.has_repos()), REPOS).await;
        let Some(git) = app.git.get().cloned() else {
            return bench_quit(&app);
        };
        git.create_branch();
        glib::timeout_future(Duration::from_millis(500)).await;
        let Some(dialog) = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>()
        else {
            println!("bench git_branch no_dialog");
            return bench_quit(&app);
        };
        let root = dialog.clone().upcast::<gtk::Widget>();
        let entry = find_widget(&root, &|w| w.is::<gtk::Entry>()).and_downcast::<gtk::Entry>();
        let label = find_widget(&root, &|w| {
            w.downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.has_css_class("dim-label"))
        })
        .and_downcast::<gtk::Label>();
        let (Some(entry), Some(label)) = (entry, label) else {
            println!("bench git_branch no_field");
            return bench_quit(&app);
        };
        for typed in ["", "topic", "my new branch", "fix: ~bug^ ?", "?*"] {
            entry.set_text(typed);
            println!(
                "bench git_branch typed={typed:?} shown={} says={:?} create={}",
                label.is_visible(),
                label.label(),
                dialog.is_response_enabled(crate::dialogs::CONFIRM)
            );
        }
        entry.set_text("my new branch");
        // A second for a screenshot of the dialog as it stands.
        glib::timeout_future(Duration::from_millis(1000)).await;
        dialog.emit_by_name::<()>("response", &[&crate::dialogs::CONFIRM]);
        dialog.close();
        wait_for(|| !git.busy(), 20000).await;
        let head = std::process::Command::new("git")
            .arg("-C")
            .arg(app.root())
            .args(["branch", "--show-current"])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap_or_default();
        println!("bench git_branch head={head:?}");
        bench_quit(&app);
    });
}
