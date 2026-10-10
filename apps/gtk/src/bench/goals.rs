//! The Code pane's goals at a place in a proof.

use super::*;

/// `ACCENT_BENCH_GOALS=<rel>:<line>:<col>[:<line>:<col>]` opens a Lean file, waits for its server
/// to say it answers goals, puts the caret at the 1-based `line` and `col` and brings the Code
/// pane up, then waits up to a minute for the pane's answer and prints `bench goals <n> <first
/// goal's last line>`, `none` where it shows no goals, and how long the answer took. With a second
/// place it then hides the sidebar, moves the caret there, which asks nothing while the pane is
/// out of sight, and shows the sidebar again, printing what the pane asks for as it comes back
/// (`bench goals shown …`). Meanwhile it prints the lines the gutter marks as still being checked
/// each time they change, 1-based, and quits once the check is done ([`watch_checking`]).
pub(super) fn bench_goals(app: &Rc<App>, arg: &str) {
    let (rel, places) = arg.split_once(':').unwrap_or((arg, ""));
    let places: Option<Vec<accent_api::Pos>> = places
        .split(':')
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|place| match place {
            [line, col] => Some(accent_api::Pos {
                line: line.parse::<u32>().ok()?.saturating_sub(1),
                character: col.parse::<u32>().ok()?.saturating_sub(1),
            }),
            _ => None,
        })
        .collect();
    let Some(places) = places else {
        return bench_quit(app);
    };
    app.open_path(rel);
    let watching = glib::spawn_future_local(watch_checking(app.clone()));
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
        tab.buffer
            .place_cursor(&lang::iter_at(&tab.buffer, places[0]));
        let asked = Instant::now();
        app.show_pane("code");
        let goals = goals_after(sidebar, None).await;
        say("goals", goals.as_deref());
        println!("bench goals ms {:.0}", ms_since(asked));
        if let Some(&again) = places.get(1) {
            app.sidebar_column.set_visible(false);
            tab.buffer.place_cursor(&lang::iter_at(&tab.buffer, again));
            glib::timeout_future(Duration::from_secs(1)).await;
            app.sidebar_column.set_visible(true);
            say("goals shown", goals_after(sidebar, goals).await.as_deref());
        }
        let _ = watching.await;
        bench_quit(&app);
    });
}

/// `bench checking <ms> 6-9,11,13-14` each time the active tab's gutter marks other lines as
/// still being checked, and `bench checking <ms> done` once a check it saw is over, waited for up
/// to a minute.
async fn watch_checking(app: Rc<App>) {
    let started = Instant::now();
    let mut shown = Vec::new();
    while started.elapsed() < Duration::from_secs(60) {
        glib::timeout_future(Duration::from_millis(20)).await;
        let lines = app.active().map(|tab| tab.checking()).unwrap_or_default();
        if lines == shown {
            continue;
        }
        let ms = ms_since(started);
        if lines.is_empty() {
            println!("bench checking {ms:.0} done");
            return;
        }
        let ranges: Vec<String> = lines
            .iter()
            .map(|r| match r.len() {
                1 => format!("{}", r.end),
                _ => format!("{}-{}", r.start + 1, r.end),
            })
            .collect();
        println!("bench checking {ms:.0} {}", ranges.join(","));
        shown = lines;
    }
}

/// The goals the pane shows once they are other than `before`, waited for up to a minute.
async fn goals_after(
    sidebar: &crate::sidebar::Sidebar,
    before: Option<Vec<String>>,
) -> Option<Vec<String>> {
    let asked = Instant::now();
    let mut goals = before.clone();
    while goals == before && asked.elapsed() < Duration::from_secs(60) {
        glib::timeout_future(Duration::from_millis(100)).await;
        goals = sidebar.shown_goals();
    }
    goals
}

/// `bench <what> <n> <first goal's last line>`, or `none`.
fn say(what: &str, goals: Option<&[String]>) {
    match goals {
        Some(goals) => {
            let last = goals.first().and_then(|goal| goal.lines().last());
            let line = format!("bench {what} {} {}", goals.len(), last.unwrap_or_default());
            println!("{}", line.trim_end());
        }
        None => println!("bench {what} none"),
    }
}
