//! A merge view over a conflict a real `git merge` left, opened from its Merge Conflicts row.

use super::*;
use crate::diff::merge::BASE;

/// The merge view end to end (`=merge:<rel>`). It makes a repository in the vault root and merges
/// two branches that change `<rel>`, sixty lines, at lines 10, 30 and 50 both, and line 58 on the
/// incoming side alone, which git merges by itself; the block at line 30 is then given a base, as
/// `merge.conflictStyle = diff3` writes one. It activates the file's Merge Conflicts row and
/// prints, once the view has settled: the side columns' titles, the rows, the blocks, the hidden
/// runs and the buttons, and `misaligned=0`, the claim, with the caret's line (`opened`); where
/// each column's button on the first block's strip sits (`strip`, one height); how many lines of
/// each side are tinted (`tints`), every side line beside a block among them; where each connector
/// between the columns ends, against where the rows it joins are drawn (`links`, `off=0` the
/// claim), holding three seconds for a screenshot (`hold`); a word typed into the first block
/// (`typed`); the first block taken from the left column's arrow (`current`, and `tints` again),
/// the next by Accept Incoming from the palette with the caret in it (`incoming`), the last by the
/// middle's Both (`both`), each with the lines it left; the left column switched to the base
/// (`base`); Show All Unchanged Lines down and up (`all`); a divider dragged (`resize`, and `links`
/// again), and the view scrolled (`scrolled`, the connectors once more); and the view left
/// (`left`). Point it at a throwaway vault.
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
        println!("bench compare_merge strip {:?}", merge.block_centres(0));
        println!("bench compare_merge tints {:?}", merge.tinted());
        let links = || {
            let ([left, right], off) = merge.links();
            format!("left={left:?} right={right:?} off={off}")
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

        merge.press(0, 0);
        wait(800).await;
        println!("bench compare_merge current {:?} {}", line(10), state());
        println!("bench compare_merge tints {:?}", merge.tinted());

        let first = accent_core::conflict::blocks(&tab.text())
            .first()
            .map(|b| b.ours.start);
        if let Some(at) = first {
            let chars = tab.text()[..at].chars().count() as i32;
            tab.buffer.place_cursor(&tab.buffer.iter_at_offset(chars));
        }
        let _ = WidgetExt::activate_action(&app.window, "win.conflict-incoming", None);
        wait(800).await;
        println!(
            "bench compare_merge incoming {:?} {}",
            [line(30), line(31)],
            state()
        );

        merge.press(0, 1);
        wait(800).await;
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
        println!(
            "bench compare_merge left merging={} conflicts={}",
            tab.merging().is_some(),
            accent_core::conflict::blocks(&tab.text()).len()
        );
        bench_quit(&app);
    });
}
