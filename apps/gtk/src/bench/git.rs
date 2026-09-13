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
        if let Some(model) = view.and_then(|v| v.model()) {
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
        for answer in ["discard", "delete"] {
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
            dialog.emit_by_name::<()>("response", &[&answer]);
            dialog.close();
            settle().await;
        }
        println!("bench git_changes_rows {}", git.changes_rows());
        bench_quit(&app);
    });
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
