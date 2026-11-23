//! `ACCENT_BENCH_PREVIEW=tasks`: a click on a task's box in a note's preview, through XTEST.

use super::git::xtest;
use super::*;

const NOTE: &str = "tasks-drill.md";
const ORG: &str = "tasks-drill.org";
const OTHER: &str = "tasks-other.md";

/// A box of each kind, one inside a quote, and a link in a task's text, over a page that scrolls.
fn note() -> String {
    let filler: String = (1..=40)
        .map(|i| format!("Filler paragraph {i}, there for the page to scroll.\n\n"))
        .collect();
    format!(
        "# Tasks\n\n- [ ] open, see [[tasks-other]] or [[#Far]]\n- [x] done\n- [X] upper\n\
         - [/] doing\n> - [ ] quoted\n\n{filler}## Far\n"
    )
}

/// The preview beside its note, the caret at the note's end: each box clicked, its line printed
/// with whether the page stayed where it was read and the pointer over the box, then one `Ctrl+Z`
/// over the page, after which the note is as it was, and on the first box a Redo and an Undo. A drag across a box changes nothing, nor do clicks on the
/// links in a task's text: one to a heading of the note brings the page there, the other opens
/// its note. Then an org note's preview alone in its pane:
/// each checkbox clicked, saved; and presented, where a click changes nothing.
pub(super) async fn run(app: &Rc<App>) {
    let root = app.root();
    let written = std::fs::write(root.join(NOTE), note())
        .and(std::fs::write(root.join(OTHER), "# Other\n"))
        .and(std::fs::write(
            root.join(ORG),
            "* Tasks\n- [ ] open\n- [-] doing\n- [X] done\n",
        ));
    if let Err(e) = written {
        return println!("bench tasks wrote=false {e}");
    }
    until("tasks", "reconciled", 30_000, || app.reconciled.get()).await;
    app.open_path(NOTE);
    super::preview::beside(app).await;
    let (Some(rendered), Some(tab)) = (settled(app).await, app.tab_for(NOTE)) else {
        return println!("bench tasks preview=none");
    };
    tab.buffer.place_cursor(&tab.buffer.end_iter());
    let screen = |(x, y): (f64, f64)| {
        let (sx, sy) = app.window.surface_transform();
        format!("{} {}", x + sx, y + sy)
    };
    let with = |f: &dyn Fn(&crate::preview::Preview) -> Option<(f64, f64)>| {
        rendered.preview().as_ref().and_then(f)
    };
    // The page followed the caret to the end: back to its top, where the boxes are.
    glib::timeout_future(Duration::from_millis(800)).await;
    rendered.scroll_to_line(1);
    glib::timeout_future(Duration::from_millis(400)).await;
    let boxes = || (rendered.preview().as_ref()).map_or_else(Vec::new, |p| p.boxes());
    if let Some((_, at)) = boxes().first() {
        xtest(&format!("move {}; focus", screen(*at))).await;
    }
    for i in 0..boxes().len() {
        let Some((line, at)) = boxes().get(i).copied() else {
            break;
        };
        let before = tab.text();
        let top = (rendered.preview().as_ref()).map_or(0.0, |p| p.scroll_y());
        xtest(&format!("move {}", screen(at))).await;
        let cursor = (rendered.preview().as_ref()).and_then(|p| p.cursor());
        xtest("down; up").await;
        let after = tab.text();
        let moved = (rendered.preview().as_ref()).map_or(0.0, |p| p.scroll_y()) - top;
        println!(
            "bench tasks click line={line} {:?} cursor={cursor:?} moved={}",
            after.lines().nth(line as usize - 1).unwrap_or_default(),
            moved.abs() > 1.0
        );
        // Undo over the page, which has kept the keyboard, steps the note's history; on the first
        // box Redo too, and Undo again.
        xtest("key ctrl+z").await;
        println!(
            "bench tasks undo line={line} restored={}",
            tab.text() == before
        );
        if i == 0 {
            xtest("key ctrl+shift+z").await;
            let redone = tab.text() == after;
            xtest("key ctrl+z").await;
            println!(
                "bench tasks redo line={line} redone={redone} restored={}",
                tab.text() == before
            );
        }
        // The page shows the note again before the next click, which it would otherwise refuse.
        until("tasks", "rendered", 5_000, || {
            (rendered.preview().as_ref()).is_some_and(|p| p.shows(&tab.text()))
        })
        .await;
        // And laid out, where a box is drawn rather than estimated.
        glib::timeout_future(Duration::from_millis(300)).await;
    }

    // A drag over the first box selects; a click on the link in its text follows it.
    let before = tab.text();
    let first = with(&|p| p.boxes().first().map(|(_, at)| *at));
    if let Some((x, y)) = first {
        let (from, to) = (screen((x - 20.0, y)), screen((x + 60.0, y)));
        xtest(&format!("drag {from} {to}")).await;
        println!("bench tasks drag changed={}", tab.text() != before);
    }
    if let Some(at) = with(&|p| p.point_of("Far")) {
        let top = (rendered.preview().as_ref()).map_or(0.0, |p| p.scroll_y());
        xtest(&format!("move {}; down; up", screen(at))).await;
        let moved = (rendered.preview().as_ref()).map_or(0.0, |p| p.scroll_y()) - top;
        println!(
            "bench tasks heading moved={} changed={}",
            moved > 100.0,
            tab.text() != before
        );
        rendered.scroll_to_line(1);
        glib::timeout_future(Duration::from_millis(400)).await;
    }
    if let Some(at) = with(&|p| p.point_of("tasks-other")) {
        xtest(&format!("move {}; down; up", screen(at))).await;
        println!(
            "bench tasks link opened={} changed={}",
            app.tab_for(OTHER).is_some(),
            tab.text() != before
        );
    }

    // The org note's preview alone in front of its pane, its note behind it.
    app.open_path(ORG);
    until("tasks", "org", 10_000, || {
        app.active().is_some_and(|t| t.rel() == ORG)
    })
    .await;
    app.toggle_preview();
    let Some(org) = app.tab_for(ORG) else {
        return println!("bench tasks org=none");
    };
    let shown = until("tasks", "org preview", 10_000, || {
        let rendered = app.rendered_of(&org.page)?;
        let ready =
            (rendered.preview().as_ref()).is_some_and(|p| p.settled() && p.boxes().len() == 3);
        ready.then_some(rendered)
    })
    .await;
    let Some(rendered) = shown else {
        return;
    };
    let alone = app
        .pane_of(&rendered.page)
        .is_some_and(|pane| pane.has(&org.page));
    let org_boxes = (rendered.preview().as_ref()).map_or_else(Vec::new, |p| p.boxes());
    for (line, at) in org_boxes {
        xtest(&format!("move {}; down; up", screen(at))).await;
        let text = org.text();
        println!(
            "bench tasks org alone={alone} line={line} {:?}",
            text.lines().nth(line as usize - 1).unwrap_or_default()
        );
        until("tasks", "org rendered", 5_000, || {
            (rendered.preview().as_ref()).is_some_and(|p| p.shows(&org.text()))
        })
        .await;
    }
    let saved = until("tasks", "org saved", 10_000, || {
        std::fs::read_to_string(root.join(ORG)).is_ok_and(|t| t == org.text())
    })
    .await;
    println!("bench tasks org saved={}", saved.is_some());

    // Presented, its boxes are only read.
    app.set_presenting(true);
    glib::timeout_future(Duration::from_millis(600)).await;
    let before = org.text();
    let at = (rendered.preview().as_ref()).and_then(|p| p.boxes().first().map(|(_, at)| *at));
    if let Some(at) = at {
        xtest(&format!("move {}", screen(at))).await;
        let cursor = (rendered.preview().as_ref()).and_then(|p| p.cursor());
        xtest("down; up").await;
        println!(
            "bench tasks presented cursor={cursor:?} changed={}",
            org.text() != before
        );
    }
    app.set_presenting(false);
}

/// The preview tab's page, once its render is in.
async fn settled(app: &Rc<App>) -> Option<Rc<crate::rendered::Rendered>> {
    until("tasks", "preview", 10_000, || {
        let rendered = super::preview::shown(app)?;
        let ready =
            (rendered.preview().as_ref()).is_some_and(|p| p.settled() && p.boxes().len() == 5);
        ready.then_some(rendered)
    })
    .await
}
