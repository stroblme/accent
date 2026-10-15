//! Open a real Git merge conflict from its Merge Conflicts row.

use super::*;
use crate::diff::merge::BASE;

/// `=merge:<rel>` creates a throwaway repository and merges a 60-line file with conflicts
/// at lines 10, 30 and 50, an incoming-only line 58 and a diff3 base in the middle block.
///
/// Report titles, rows, blocks, hidden runs, controls, tints, widths and connectors. Expected
/// alignment: `misaligned=0`, strip `over=0 stale=0`, connectors `off=0`. Hold 3 s for a
/// screenshot.
///
/// Type in the first block, then Accept Current, Incoming and Both through XTEST. Each `aim <x>
/// <y>` gives a button centre for `build-aux/xtest.py :N "move <x> <y>; focus; down; up"` within
/// 10 s. Switch to base, toggle unchanged lines, resize and scroll, checking connectors again.
///
/// Reopen and restore the view, undo resolutions, exercise both Accept All commands and their
/// undo, then Mark Resolved. A second autostash merge checks conversion of a working-tree
/// comparison (`switched`) and a deleted incoming side (`deleted`).
pub(in crate::bench) fn bench_compare_merge(app: &Rc<App>, rel: &str) {
    app.show_pane("git");
    let (app, rel) = (app.clone(), rel.to_string());
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
                .is_ok_and(|out| out.status.success())
        };
        let write = |text: &str| std::fs::write(root.join(&rel), text).is_ok();
        let base: Vec<String> = (1..=60)
            .map(|i| match i % 7 {
                0 => format!("line {i} {}", "wrapping words ".repeat(12)),
                _ => format!("line {i}"),
            })
            .collect();
        let text = |changes: &[(usize, &str)]| -> String {
            let mut lines = base.clone();
            for (n, to) in changes {
                lines[n - 1] = to.to_string();
            }
            lines.iter().map(|line| format!("{line}\n")).collect()
        };
        let long = format!("line 50 main {}", "and a long line ".repeat(10));
        let made = write(&text(&[]))
            && git(&["init", "-q", "-b", "main"])
            && git(&["add", "--", &rel])
            && git(&["commit", "-qm", "base"])
            && git(&["checkout", "-q", "-b", "side"])
            && write(&text(&[
                (10, "line 10 side"),
                (30, "line 30 side\nline 30 side extra"),
                (50, "line 50 side"),
                (58, "line 58 from side"),
            ]))
            && git(&["commit", "-qam", "side"])
            && git(&["checkout", "-q", "main"])
            && write(&text(&[
                (10, "line 10 main"),
                (30, "line 30 main"),
                (50, &long),
            ]))
            && git(&["commit", "-qam", "main"]);
        let conflicted = !git(&["merge", "-q", "side"]);
        let Ok(file) = std::fs::read_to_string(root.join(&rel)) else {
            println!("bench compare_merge unreadable");
            return bench_quit(&app);
        };
        let diff3 = file.replace(
            "<<<<<<< HEAD\nline 30 main\n=======",
            "<<<<<<< HEAD\nline 30 main\n||||||| base\nline 30\n=======",
        );
        println!(
            "bench compare_merge made={made} conflicted={conflicted} diff3={}",
            write(&diff3)
        );

        // The debounced refresh and its `git status`.
        wait(2500).await;
        for _ in 0..120 {
            if app.git.get().is_some_and(|git| git.changes_rows() > 0) {
                break;
            }
            wait(250).await;
        }
        let Some(git) = app.git.get().cloned() else {
            println!("bench compare_merge no_repo");
            return bench_quit(&app);
        };
        let section = git.activate_change(&rel);
        let mut merge = None;
        for _ in 0..80 {
            wait(100).await;
            merge = app.tab_for(&rel).and_then(|tab| tab.merging());
            if merge.as_ref().is_some_and(|m| m.settled()) {
                break;
            }
        }
        let (Some(tab), Some(merge)) = (app.tab_for(&rel), merge) else {
            println!("bench compare_merge section={section:?} merging=false");
            return bench_quit(&app);
        };
        wait(500).await;
        let caret_line = || tab.buffer.iter_at_mark(&tab.buffer.get_insert()).line() + 1;
        let state = || {
            let (rows, blocks, hidden, buttons) = merge.counts();
            format!(
                "rows={rows} blocks={blocks} hidden={hidden} buttons={buttons} misaligned={}",
                merge.misaligned()
            )
        };
        let line = |n: i32| -> String {
            let text = tab.text();
            text.lines()
                .nth(n as usize - 1)
                .unwrap_or_default()
                .to_string()
        };
        println!(
            "bench compare_merge opened section={section:?} titles={:?} {} caret_line={} {:?}",
            merge.titles(),
            state(),
            caret_line(),
            line(caret_line()),
        );
        println!("bench compare_merge strip {}", merge.strip());
        println!("bench compare_merge tints {:?}", merge.tinted());
        let (columns, strips) = merge.widths();
        println!("bench compare_merge widths columns={columns:?} strips={strips:?}");
        let links = || {
            let (ends, off) = merge.links();
            format!("left={:?} right={:?} off={off}", ends[0], ends[1])
        };
        println!("bench compare_merge links {}", links());
        // For a screenshot.
        println!("bench compare_merge hold");
        wait(3000).await;

        // A word typed at the end of the first block's current side.
        if let Some(at) = tab.buffer.iter_at_line(10) {
            let mut end = at;
            end.forward_to_line_end();
            tab.buffer.place_cursor(&end);
            tab.buffer.insert_at_cursor(" typed");
        }
        wait(800).await;
        println!("bench compare_merge typed {:?} {}", line(11), state());

        // The first block's button named `label`, scrolled halfway down the view and pressed
        // through the real pointer.
        let press = async |label: &str| {
            let Some((top, button)) = merge.strip_buttons(label).first().cloned() else {
                return println!("bench compare_merge {label:?} none");
            };
            let view = merge.view(0);
            if let Some(adj) = view.vadjustment() {
                let seen = view.visible_rect();
                adj.set_value(adj.value() + f64::from(top - seen.y()) - adj.page_size() / 2.0);
            }
            wait(500).await;
            println!("bench compare_merge strip {}", merge.strip());
            let middle = gtk::graphene::Point::new(
                button.width() as f32 / 2.0,
                button.height() as f32 / 2.0,
            );
            let (sx, sy) = app.window.surface_transform();
            if let Some(p) = button
                .root()
                .and_then(|root| button.compute_point(&root, &middle))
            {
                println!(
                    "bench compare_merge aim {:.0} {:.0}",
                    f64::from(p.x()) + sx,
                    f64::from(p.y()) + sy
                );
            }
            let blocks = merge.counts().1;
            for _ in 0..100 {
                wait(100).await;
                if merge.counts().1 != blocks {
                    break;
                }
            }
            wait(800).await;
        };
        press("Accept Current").await;
        println!("bench compare_merge current {:?} {}", line(10), state());
        println!("bench compare_merge tints {:?}", merge.tinted());

        press("Accept Incoming").await;
        println!(
            "bench compare_merge incoming {:?} {}",
            [line(30), line(31)],
            state()
        );

        press("Accept Both").await;
        let both: Vec<String> = (51..=52).map(line).collect();
        println!("bench compare_merge both {both:?} {}", state());

        merge.show_stage(0, BASE);
        wait(800).await;
        println!(
            "bench compare_merge base titles={:?} {}",
            merge.titles(),
            state()
        );

        for on in [true, false] {
            merge.unfold().set_active(on);
            wait(800).await;
            println!("bench compare_merge all on={on} {}", state());
        }

        let paned = merge.paned();
        paned.set_position(paned.position() - 150);
        wait(1500).await;
        println!("bench compare_merge resize {}", state());
        println!("bench compare_merge links {}", links());
        if let Some(scroll) = merge.view(0).vadjustment() {
            scroll.set_value(scroll.value() + 120.0);
        }
        wait(300).await;
        println!("bench compare_merge scrolled {}", links());

        tab.leave_compare();
        wait(300).await;
        let blocks = || accent_core::conflict::blocks(&tab.text()).len();
        println!(
            "bench compare_merge left merging={} conflicts={}",
            tab.merging().is_some(),
            blocks()
        );

        // Open from its row again, which the session keeps, and put back as a restore does.
        let merging = || tab.merging().is_some_and(|m| m.settled());
        // The merge on screen now, the one above having been left.
        let now = || {
            tab.merging().map_or("merging=false".to_string(), |m| {
                format!("blocks={} misaligned={}", m.counts().1, m.misaligned())
            })
        };
        git.activate_change(&rel);
        for _ in 0..80 {
            wait(100).await;
            if merging() {
                break;
            }
        }
        let kept = app.current_session().compared.get(&rel).cloned();
        tab.leave_compare();
        if let Some(what) = kept.clone() {
            git.restore(what, || {});
        }
        for _ in 0..80 {
            wait(100).await;
            if merging() {
                break;
            }
        }
        println!(
            "bench compare_merge restored kept={:?} merging={}",
            kept.map(|what| what.sides),
            merging()
        );

        // The three blocks back by undo, then each side of all of them at once, one undo step.
        for _ in 0..3 {
            tab.buffer.undo();
        }
        wait(800).await;
        println!("bench compare_merge undone blocks={}", blocks());
        let act = |name: &str| WidgetExt::activate_action(&app.window, name, None).is_ok();
        let all = act("win.conflict-all-incoming");
        wait(800).await;
        let lines: Vec<String> = [10, 30, 31, 51].into_iter().map(line).collect();
        println!("bench compare_merge all_incoming {all} {lines:?} {}", now());
        tab.buffer.undo();
        wait(800).await;
        println!("bench compare_merge all_undone blocks={}", blocks());
        let all = act("win.conflict-all-current");
        wait(800).await;
        let lines: Vec<String> = [10, 30, 50].into_iter().map(line).collect();
        println!("bench compare_merge all_current {all} {lines:?} {}", now());

        // Mark Resolved on a file with no block left: saved, staged, and the view gone with the
        // refresh after.
        let resolved = act("win.merge-resolved");
        for _ in 0..100 {
            wait(100).await;
            if tab.merging().is_none() {
                break;
            }
        }
        // `git` is the pane by now: what it printed, `None` where it failed.
        let sh = |args: &[&str]| {
            let out = std::process::Command::new("git")
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
                .ok()?;
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            out.status.success().then_some(text)
        };
        println!(
            "bench compare_merge resolved {resolved} merging={} unmerged={:?} staged={:?}",
            tab.merging().is_some(),
            sh(&["ls-files", "-u", "--", &rel]),
            sh(&["diff", "--cached", "--name-only"]),
        );

        // A working-tree comparison of `b.md` that the next merge leaves unmerged becomes its
        // merge; `c.md`, deleted on the incoming side, opens as the file with a toast.
        let text = |n: usize, at: &str| -> String {
            (1..=n)
                .map(|i| format!("{} {i}\n", if i == 3 { at } else { "line" }))
                .collect()
        };
        let write = |rel: &str, text: &str| std::fs::write(root.join(rel), text).is_ok();
        let ok = |args: &[&str]| sh(args).is_some();
        let made = ok(&["commit", "-qm", "merged"])
            && write("b.md", &text(20, "line"))
            && write("c.md", &text(5, "line"))
            && ok(&["add", "--", "b.md", "c.md"])
            && ok(&["commit", "-qm", "two more"])
            && ok(&["checkout", "-q", "-b", "side2"])
            && write("b.md", &text(20, "side"))
            && ok(&["rm", "-q", "c.md"])
            && ok(&["commit", "-qam", "side2"])
            && ok(&["checkout", "-q", "main"])
            && write("b.md", &text(20, "main"))
            && write("c.md", &text(5, "main"))
            && ok(&["commit", "-qam", "main again"])
            && write("b.md", &text(20, "main").replace("line 15", "line fifteen"));
        wait(2500).await;
        git.compare_worktree("b.md");
        let b = || app.tab_for("b.md");
        for _ in 0..80 {
            wait(100).await;
            if b().and_then(|tab| tab.comparison()).is_some() {
                break;
            }
        }
        let compared = b().and_then(|tab| tab.comparison()).is_some();
        let conflicted = !ok(&["merge", "--autostash", "-q", "side2"]);
        for _ in 0..100 {
            wait(100).await;
            if b()
                .and_then(|tab| tab.merging())
                .is_some_and(|m| m.settled())
            {
                break;
            }
        }
        let switched = b().and_then(|tab| tab.merging());
        println!(
            "bench compare_merge switched made={made} compared={compared} \
             conflicted={conflicted} merging={} comparison={} titles={:?}",
            switched.is_some(),
            b().and_then(|tab| tab.comparison()).is_some(),
            switched.map(|m| m.titles()),
        );
        app.toasts.dismiss_all();
        let section = git.activate_change("c.md");
        wait(1500).await;
        println!(
            "bench compare_merge deleted section={section:?} merging={} toasts={:?}",
            app.tab_for("c.md").and_then(|tab| tab.merging()).is_some(),
            app.toasts.shown()
        );
        bench_quit(&app);
    });
}
