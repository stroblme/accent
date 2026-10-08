//! The minimap's drills: what the map costs a scrolling frame.

use super::*;

/// How many scroll steps a `frames` round takes, each a quarter of a page.
const STEPS: usize = 120;

/// `ACCENT_BENCH_MINIMAP=frames:<rel>` fills the note at `rel` with 256 KB of sections and
/// scrolls it down by a quarter page [`STEPS`] times, one step per frame, with the minimap off and
/// then on, printing each round's median and worst frame: `paint_ms` from the end of the layout
/// phase to the end of painting (the snapshot and the render), `frame_ms` from the frame's start
/// to the end of painting. Fills the note, so point it at a scratch vault.
pub(super) fn bench_minimap(app: &Rc<App>, arg: &str) {
    scratch_only(app, "ACCENT_BENCH_MINIMAP");
    let Some(("frames", rel)) = arg.split_once(':') else {
        eprintln!("ACCENT_BENCH_MINIMAP=frames:<rel>");
        return bench_quit(app);
    };
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let section = super::style::TYPING_SECTION;
        tab.set_text(&section.repeat(256 * 1024 / section.len()));
        for on in [false, true] {
            tab.set_minimap(on);
            frames(&app, &tab, if on { "on" } else { "off" }).await;
        }
        bench_quit(&app);
    });
}

/// One round of [`bench_minimap`]'s `frames`, from the top of the note.
async fn frames(app: &Rc<App>, tab: &Rc<Tab>, map: &str) {
    let Some(adj) = tab.view.vadjustment() else {
        return;
    };
    adj.set_value(0.0);
    glib::timeout_future(Duration::from_secs(2)).await;
    let Some(clock) = app.window.frame_clock() else {
        return;
    };
    // Connected after GTK's own handlers: a before-paint's mark is where the frame starts, a
    // layout's where its painting starts, and a paint's where that ends.
    let marks = Rc::new(RefCell::new(Vec::new()));
    let mark = |phase: char| {
        let marks = marks.clone();
        move |_: &gdk::FrameClock| marks.borrow_mut().push((phase, Instant::now()))
    };
    let handlers = [
        clock.connect_before_paint(mark('b')),
        clock.connect_layout(mark('l')),
        clock.connect_paint(mark('p')),
    ];
    for _ in 0..STEPS {
        let painted = marks.borrow().iter().filter(|m| m.0 == 'p').count();
        adj.set_value(adj.value() + adj.page_size() / 4.0);
        for _ in 0..100 {
            glib::timeout_future(Duration::from_millis(2)).await;
            if marks.borrow().iter().filter(|m| m.0 == 'p').count() > painted {
                break;
            }
        }
    }
    for handler in handlers {
        clock.disconnect(handler);
    }
    let marks = marks.take();
    let between = |from: char, to: char| -> Vec<f64> {
        let mut spans: Vec<f64> = marks
            .iter()
            .enumerate()
            .filter(|(_, m)| m.0 == from)
            .filter_map(|(at, m)| {
                let end = marks[at..].iter().find(|e| e.0 == to)?;
                Some((end.1 - m.1).as_secs_f64() * 1000.0)
            })
            .collect();
        spans.sort_by(f64::total_cmp);
        spans
    };
    let say = |spans: &[f64]| {
        let median = spans.get(spans.len() / 2).copied().unwrap_or(f64::NAN);
        let worst = spans.last().copied().unwrap_or(f64::NAN);
        format!("{median:.2}/{worst:.2}")
    };
    println!(
        "bench minimap frames map={map} steps={STEPS} frames={} paint_ms={} frame_ms={}",
        marks.iter().filter(|m| m.0 == 'p').count(),
        say(&between('l', 'p')),
        say(&between('b', 'p')),
    );
}
