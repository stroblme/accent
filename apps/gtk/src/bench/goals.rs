//! The Code pane's goals at a place in a proof.

use super::*;

/// `ACCENT_BENCH_GOALS=<rel>:<line>:<col>` opens a Lean file, waits for its server to say it
/// answers goals, puts the caret at the 1-based `line` and `col` and brings the Code pane up, then
/// waits up to a minute for the pane's answer and prints `bench goals <n> <first goal's last
/// line>`, `none` where it shows no goals, and how long the answer took.
pub(super) fn bench_goals(app: &Rc<App>, arg: &str) {
    let mut parts = arg.rsplitn(3, ':');
    let (Some(col), Some(line), Some(rel)) = (parts.next(), parts.next(), parts.next()) else {
        return bench_quit(app);
    };
    let (Ok(line), Ok(col)) = (line.parse::<u32>(), col.parse::<u32>()) else {
        return bench_quit(app);
    };
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        let sidebar = app.sidebar.get().expect("a sidebar");
        let mut tab = None;
        for _ in 0..300 {
            glib::timeout_future(Duration::from_millis(200)).await;
            tab = app.active().filter(|_| sidebar.has_pane("code"));
            if tab.is_some() {
                break;
            }
        }
        let Some(tab) = tab else {
            println!("bench goals no_pane");
            return bench_quit(&app);
        };
        let at = accent_api::Pos {
            line: line.saturating_sub(1),
            character: col.saturating_sub(1),
        };
        tab.buffer.place_cursor(&lang::iter_at(&tab.buffer, at));
        let asked = Instant::now();
        app.show_pane("code");
        let mut goals = None;
        while goals.is_none() && asked.elapsed() < Duration::from_secs(60) {
            glib::timeout_future(Duration::from_millis(100)).await;
            goals = sidebar.shown_goals();
        }
        match goals {
            Some(goals) => {
                let last = goals.first().and_then(|goal| goal.lines().last());
                let line = format!("bench goals {} {}", goals.len(), last.unwrap_or_default());
                println!("{}", line.trim_end());
            }
            None => println!("bench goals none"),
        }
        println!("bench goals ms {:.0}", ms_since(asked));
        bench_quit(&app);
    });
}
