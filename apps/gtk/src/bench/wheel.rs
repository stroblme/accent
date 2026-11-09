//! `ACCENT_BENCH_WHEEL=<rel>`: how far three wheel notches sent through XTEST scroll each kind of
//! scroller the reader meets, against GTK's step of `page^(2/3)` a notch: the note `<rel>`, the
//! Files tree with `Daily` open, the switcher's list (Open Recent's) and Preferences.
//! A line gives the distance, the frames it took, the worst gap between them and a frame's median
//! layout and paint. Then a touchpad's scroll as GTK applies one (`unclamped + 2.5 × delta`, a
//! frame at a time) for a second down and a second up from the middle, at a delta of 1 and of 0.3
//! a frame, by GTK alone and with `Wheel::touchpad` carrying what a list rounds away.

use super::*;

/// Notches each surface is sent, down.
const NOTCHES: i32 = 3;
/// A touchpad's frames each way, and the pixels GTK moves a frame: 2.5 for a delta of one.
const TOUCHPAD_FRAMES: u32 = 60;
const TOUCHPAD_PX: [f64; 2] = [2.5, 0.75];

pub(super) fn bench_wheel(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_secs(2)).await;
        let editor = app
            .active()
            .and_then(|tab| tab.view.ancestor(gtk::ScrolledWindow::static_type()));
        measure(&app, "editor", editor).await;

        app.show_pane("files");
        let tree = app.tree.get().expect("a tree");
        until("wheel", "reveal Daily", 10_000, || tree.reveal("Daily")).await;
        if let Some(row) = tree::find_row(tree.model(), "Daily") {
            row.set_expanded(true);
        }
        glib::timeout_future(Duration::from_secs(1)).await;
        measure(&app, "files", tree.view().parent()).await;

        let _ = WidgetExt::activate_action(&app.window, "win.palette-commands", None);
        glib::timeout_future(Duration::from_secs(1)).await;
        measure(
            &app,
            "palette",
            in_dialog(&app, |w| w.is::<gtk::ListView>()),
        )
        .await;
        if let Some(dialog) = app.window.visible_dialog() {
            dialog.force_close();
        }

        let _ = WidgetExt::activate_action(&app.window, "win.preferences", None);
        glib::timeout_future(Duration::from_secs(1)).await;
        let page = in_dialog(&app, |w| w.is::<adw::PreferencesPage>());
        measure(&app, "preferences", page.and_then(|p| p.first_child())).await;
        bench_quit(&app);
    });
}

/// The parent of the first widget in the open dialog that `found` accepts.
fn in_dialog(app: &Rc<App>, found: impl Fn(&gtk::Widget) -> bool) -> Option<gtk::Widget> {
    let dialog = app.window.visible_dialog()?;
    let hit = find_widget(dialog.upcast_ref(), &found)?;
    match hit.is::<gtk::ListView>() {
        true => hit.parent(),
        false => Some(hit),
    }
}

async fn measure(app: &Rc<App>, name: &str, scroller: Option<gtk::Widget>) {
    let Some(scroller) = scroller.and_downcast::<gtk::ScrolledWindow>() else {
        println!("bench wheel {name} no_scroller");
        return;
    };
    let adj = scroller.vadjustment();
    adj.set_value(adj.lower());
    glib::timeout_future(Duration::from_millis(500)).await;
    let Some(clock) = app.window.frame_clock() else {
        return;
    };
    // Each frame's end, scroll, and its styling and layout (from the update's end to the layout's)
    // and painting (on to the paint's) in ms: connected after GTK's own handlers.
    let paints = Rc::new(RefCell::new(Vec::<(Instant, f64, f64, f64)>::new()));
    let marks = Rc::new(Cell::new([Instant::now(); 2]));
    let mark = |i: usize| {
        let marks = marks.clone();
        move |_: &gdk::FrameClock| {
            let mut m = marks.get();
            m[i] = Instant::now();
            marks.set(m);
        }
    };
    let handlers = [
        clock.connect_update(mark(0)),
        clock.connect_layout(mark(1)),
        clock.connect_paint({
            let (paints, adj, marks) = (paints.clone(), adj.clone(), marks.clone());
            move |_| {
                let ([u, l], now) = (marks.get(), Instant::now());
                let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
                paints
                    .borrow_mut()
                    .push((now, adj.value(), ms(u, l), ms(l, now)));
            }
        }),
    ];

    let (start, step) = (adj.value(), adj.page_size().powf(2.0 / 3.0));
    let room = adj.upper() - adj.page_size() - start;
    let middle = graphene::Point::new(
        scroller.width() as f32 / 2.0,
        scroller.height() as f32 / 2.0,
    );
    let (sx, sy) = app.window.surface_transform();
    let Some(at) = scroller.compute_point(&app.window, &middle) else {
        return;
    };
    let (x, y) = (at.x() as f64 + sx, at.y() as f64 + sy);
    super::git::xtest(&format!("move {x:.0} {y:.0}; scroll {}", -NOTCHES)).await;
    let seen = paints.take();
    let moving: Vec<&(Instant, f64, f64, f64)> = seen
        .iter()
        .scan(start, |last, p| {
            let moved = p.1 != *last;
            *last = p.1;
            Some((moved, p))
        })
        .filter(|(moved, _)| *moved)
        .map(|(_, p)| p)
        .collect();
    let worst = moving
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).as_secs_f64() * 1e3)
        .fold(0.0, f64::max);
    let median = |of: fn(&(Instant, f64, f64, f64)) -> f64| {
        let mut ms: Vec<f64> = moving.iter().map(|p| of(p)).collect();
        ms.sort_by(f64::total_cmp);
        ms.get(ms.len() / 2).copied().unwrap_or(f64::NAN)
    };
    println!(
        "bench wheel {name} moved={:.0} expected={:.0} frames={} worst_gap_ms={worst:.0} \
         layout_ms={:.1} paint_ms={:.1}",
        adj.value() - start,
        (NOTCHES as f64 * step).min(room),
        moving.len(),
        median(|p| p.2),
        median(|p| p.3),
    );

    // A second easing of this scroller's, for its touchpad hook: no real event reaches it.
    let wheel = crate::widgets::smooth_wheel(&scroller);
    for (px, carried) in TOUCHPAD_PX
        .into_iter()
        .flat_map(|px| [(px, false), (px, true)])
    {
        adj.set_value((adj.upper() - adj.page_size()) / 2.0);
        glib::timeout_future(Duration::from_millis(200)).await;
        let mut moved = [0.0; 2];
        for (i, sign) in [1.0, -1.0].into_iter().enumerate() {
            let from = adj.value();
            let left = Rc::new(Cell::new(TOUCHPAD_FRAMES));
            scroller.add_tick_callback({
                let (left, adj, wheel) = (left.clone(), adj.clone(), wheel.clone());
                move |_, _| {
                    if carried {
                        wheel.touchpad();
                    }
                    adj.set_value(adj.value() + sign * px);
                    left.set(left.get() - 1);
                    match left.get() {
                        0 => glib::ControlFlow::Break,
                        _ => glib::ControlFlow::Continue,
                    }
                }
            });
            while left.get() > 0 {
                glib::timeout_future(Duration::from_millis(50)).await;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
            moved[i] = (adj.value() - from).abs();
        }
        println!(
            "bench wheel {name} touchpad px={px} carried={carried} asked={:.0} down={:.0} up={:.0}",
            TOUCHPAD_FRAMES as f64 * px,
            moved[0],
            moved[1],
        );
    }
    for handler in handlers {
        clock.disconnect(handler);
    }
}
