//! PDF frame pacing under real wheel input and continuous adjustment motion.

use super::*;

pub(super) fn run(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let pdf = pdf::opened(&app, &rel).await.expect("PDF opened");
        let reader = pdf.views()[0]
            .clone()
            .downcast::<pdfview::PdfView>()
            .unwrap();
        let scroller = reader
            .ancestor(gtk::ScrolledWindow::static_type())
            .and_downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let adj = scroller.vadjustment();
        let clock = reader.frame_clock().unwrap();
        let frames = Rc::new(RefCell::new(Vec::new()));
        let start = Rc::new(Cell::new(Instant::now()));
        let update = clock.connect_update({
            let start = start.clone();
            move |_| start.set(Instant::now())
        });
        let paint = clock.connect_after_paint({
            let (frames, adj, start, reader) =
                (frames.clone(), adj.clone(), start.clone(), reader.clone());
            move |clock| {
                frames.borrow_mut().push((
                    clock.frame_time(),
                    adj.value(),
                    ms_since(start.get()),
                    reader.unsharp(),
                ))
            }
        });
        for scale in [1.0, 2.0] {
            pdf.set_zoom(pdfview::PdfZoom::Scale(scale));
            adj.set_value(0.0);
            assert!(!pdf::settled(&pdf, "scroll_start").await);
            let middle = graphene::Point::new(
                scroller.width() as f32 / 2.0,
                scroller.height() as f32 / 2.0,
            );
            let at = scroller.compute_point(&app.window, &middle).unwrap();
            let (sx, sy) = app.window.surface_transform();
            frames.borrow_mut().clear();
            let expected =
                (12.0 * adj.page_size().powf(2.0 / 3.0)).min(adj.upper() - adj.page_size());
            super::git::xtest(&format!(
                "move {:.0} {:.0}; scroll -12; sleep 0.3",
                at.x() as f64 + sx,
                at.y() as f64 + sy
            ))
            .await;
            report(&format!("wheel scale={scale}"), &frames.take());
            println!(
                "bench pdf scroll wheel moved={:.2} expected={expected:.2}",
                adj.value()
            );
            assert!((adj.value() - expected).abs() < 1.0, "lost wheel distance");
            for px in [0.75, 12.5] {
                adj.set_value(0.0);
                assert!(!pdf::settled(&pdf, "scroll_reset").await);
                frames.borrow_mut().clear();
                for direction in [1.0, -1.0] {
                    frames.borrow_mut().clear();
                    let expected = (adj.value() + direction * 180.0 * px)
                        .clamp(0.0, adj.upper() - adj.page_size());
                    let left = Rc::new(Cell::new(180));
                    reader.add_tick_callback({
                        let (adj, left) = (adj.clone(), left.clone());
                        move |_, _| {
                            adj.set_value(adj.value() + direction * px);
                            left.set(left.get() - 1);
                            if left.get() == 0 {
                                glib::ControlFlow::Break
                            } else {
                                glib::ControlFlow::Continue
                            }
                        }
                    });
                    while left.get() > 0 {
                        glib::timeout_future(Duration::from_millis(50)).await;
                    }
                    let seen: Vec<_> = frames
                        .take()
                        .into_iter()
                        .map(|(time, value, work, unsharp)| {
                            (time, direction * value, work, unsharp)
                        })
                        .collect();
                    report(
                        &format!("continuous scale={scale} px={px} direction={direction}"),
                        &seen,
                    );
                    assert!(
                        (adj.value() - expected).abs() < 0.01,
                        "lost scroll distance"
                    );
                }
            }
        }
        clock.disconnect(update);
        clock.disconnect(paint);
        bench_quit(&app);
    });
}

fn report(name: &str, frames: &[(i64, f64, f64, usize)]) {
    let mut gaps = Vec::new();
    let mut work = Vec::new();
    let mut unsharp = 0;
    let mut backwards = 0;
    for pair in frames.windows(2) {
        if pair[1].1 != pair[0].1 {
            gaps.push((pair[1].0 - pair[0].0) as f64 / 1000.0);
            work.push(pair[1].2);
            unsharp += usize::from(pair[1].3 > 0);
            backwards += usize::from(pair[1].1 < pair[0].1);
        }
    }
    gaps.sort_by(f64::total_cmp);
    work.sort_by(f64::total_cmp);
    let percentile = |samples: &[f64], pct: usize| {
        samples
            .get((samples.len().saturating_sub(1) * pct) / 100)
            .copied()
            .unwrap_or(0.0)
    };
    println!(
        "bench pdf scroll {name} frames={} gap_p95_ms={:.1} gap_max_ms={:.1} work_p95_ms={:.2} work_max_ms={:.2} unsharp={unsharp} backwards={backwards}",
        gaps.len(),
        percentile(&gaps, 95),
        percentile(&gaps, 100),
        percentile(&work, 95),
        percentile(&work, 100)
    );
    assert!(!gaps.is_empty(), "no scrolling frames recorded");
    assert_eq!(backwards, 0, "scroll reversed during downward input");
}
