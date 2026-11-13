//! The frames of a live drag on a diagram's largest page.

use super::*;

pub(super) fn preview(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::timeout_add_local_once(Duration::from_millis(1500), move || {
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        let file = tab.file();
        let Some((index, page)) = file
            .pages
            .iter()
            .enumerate()
            .max_by_key(|(_, p)| p.cells.len())
        else {
            return bench_quit(&app);
        };
        tab.show_page(index);
        bench_pump();
        let layers: Vec<&str> = page.layers().iter().map(|c| c.id.as_str()).collect();
        let top: Vec<String> = page
            .cells
            .iter()
            .filter(|c| c.parent.as_deref().is_some_and(|p| layers.contains(&p)))
            .map(|c| c.id.clone())
            .collect();
        let edges_on = |id: &str| {
            let on = |end: &Option<String>| end.as_deref() == Some(id);
            page.cells
                .iter()
                .filter(|c| on(&c.source) || on(&c.target))
                .count()
        };
        let busiest = top
            .iter()
            .filter(|id| page.cell(id).is_some_and(|c| c.vertex))
            .max_by_key(|id| edges_on(id))
            .cloned()
            .unwrap_or_default();
        // Each case's frames: the first, then the median and the worst of the rest.
        let run = |name: &str, ids: &[String], moving: bool| {
            let frames: Vec<(f64, f64)> = (1..=30)
                .map(|i| {
                    let delta = moving
                        .then(|| accent_drawio::Point::new(f64::from(i) * 7.0, f64::from(i) * 3.0));
                    tab.bench_move(ids, delta)
                })
                .collect();
            tab.bench_move(ids, None);
            let mut rest: Vec<f64> = frames[1..].iter().map(|f| f.0 + f.1).collect();
            rest.sort_by(f64::total_cmp);
            println!(
                "bench diagram preview {name} moved={} first_ms={:.2} (update {:.2}) median_ms={:.2} max_ms={:.2} median_update_ms={:.2}",
                ids.len(),
                frames[0].0 + frames[0].1,
                frames[0].0,
                rest[rest.len() / 2],
                rest[rest.len() - 1],
                {
                    let mut u: Vec<f64> = frames[1..].iter().map(|f| f.0).collect();
                    u.sort_by(f64::total_cmp);
                    u[u.len() / 2]
                }
            );
        };
        println!(
            "bench diagram preview page={index} cells={} busiest={busiest} edges={}",
            page.cells.len(),
            edges_on(&busiest)
        );
        run("still", &[], false);
        run("live", std::slice::from_ref(&busiest), true);
        run("boxed", &top, true);
        // The live drag again with the Properties pane out, the canvas drawn again and the main
        // loop run after each frame as a motion has them: with the busiest shape selected its
        // rows follow the drag, against nothing selected.
        app.show_pane("properties");
        let x_row = || {
            let row = find_widget(&tab.properties(), &|w| {
                w.downcast_ref::<adw::SpinRow>()
                    .is_some_and(|r| r.title() == "X")
            });
            row.and_downcast::<adw::SpinRow>()
                .map(|r| r.text().to_string())
        };
        let pumped = |selected: bool| {
            tab.select(match selected {
                true => vec![busiest.clone()],
                false => Vec::new(),
            });
            bench_pump();
            let mut ms: Vec<f64> = (1..=30)
                .map(|i| {
                    let delta = accent_drawio::Point::new(f64::from(i) * 7.0, f64::from(i) * 3.0);
                    let started = Instant::now();
                    tab.bench_move(std::slice::from_ref(&busiest), Some(delta));
                    tab.key_target().queue_draw();
                    bench_pump();
                    started.elapsed().as_secs_f64() * 1e3
                })
                .collect();
            let shown = x_row();
            tab.bench_move(&[], None);
            bench_pump();
            ms.sort_by(f64::total_cmp);
            (ms[ms.len() / 2], ms[ms.len() - 1], shown)
        };
        for _ in 0..2 {
            let (with, without) = (pumped(true), pumped(false));
            println!(
                "bench diagram preview rows median_ms={:.2} max_ms={:.2} x={:?} without median_ms={:.2} max_ms={:.2} x={:?}",
                with.0, with.1, with.2, without.0, without.1, without.2
            );
        }
        bench_quit(&app);
    });
}
