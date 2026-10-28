//! `ACCENT_BENCH_MOTION=<ipynb>`: DESIGN.md's Motion, sampled every few milliseconds
//! with animations on and then off, the setting changed while the window runs, which every motion
//! has to follow at once. A line says whether a value between a motion's ends was seen (`mid`)
//! and where it ended.
//! - The notebook: the cell buttons coming to a cell and going, and the add strips.

use super::*;
use gtk::gsk;

pub(super) fn bench_motion(app: &Rc<App>, arg: &str) {
    let (app, ipynb) = (app.clone(), arg.to_string());
    glib::spawn_future_local(async move {
        for on in [true, false] {
            if let Some(settings) = gtk::Settings::default() {
                settings.set_gtk_enable_animations(on);
            }
            let said = if on { "on" } else { "off" };
            notebook(&app, &ipynb, said).await;
        }
        bench_quit(&app);
    });
}

/// For each of the values `read` gives, whether one came over `ms` strictly between the least
/// and the most it gave, which a jump never does and a fade always does, and its last.
async fn sampled_all<const N: usize>(ms: u64, read: impl Fn() -> [f64; N]) -> [(bool, f64); N] {
    let (mut seen, t) = (vec![read()], Instant::now());
    while t.elapsed() < Duration::from_millis(ms) {
        glib::timeout_future(Duration::from_millis(4)).await;
        seen.push(read());
    }
    let last = seen[seen.len() - 1];
    std::array::from_fn(|i| {
        let least = seen.iter().map(|v| v[i]).fold(f64::MAX, f64::min);
        let most = seen.iter().map(|v| v[i]).fold(f64::MIN, f64::max);
        let between = |v: &[f64; N]| v[i] > least + 0.02 && v[i] < most - 0.02;
        (seen.iter().any(between), last[i])
    })
}

async fn sampled(ms: u64, read: impl Fn() -> f64) -> (bool, f64) {
    sampled_all(ms, || [read()]).await[0]
}

fn say(said: &str, what: &str, (mid, last): (bool, f64)) {
    println!("bench motion {said} {what} mid={mid} last={last:.2}");
}

/// `widget`'s render node, as it would be drawn now.
fn node(widget: &impl IsA<gtk::Widget>) -> Option<gsk::RenderNode> {
    let snapshot = gtk::Snapshot::new();
    let (w, h) = (f64::from(widget.width()), f64::from(widget.height()));
    gtk::WidgetPaintable::new(Some(widget)).snapshot(&snapshot, w, h);
    snapshot.to_node()
}

/// What `widget` is drawn at, its own opacity times its stylesheet's, not its children's; 0 when
/// it draws nothing. Its own is the opacity node its drawing starts with, under any offset.
fn drawn(widget: &impl IsA<gtk::Widget>) -> f64 {
    let mut at = node(widget);
    while let Some(node) = at {
        if let Some(n) = node.downcast_ref::<gsk::OpacityNode>() {
            return f64::from(n.opacity());
        }
        at = (node.downcast_ref::<gsk::TransformNode>().map(|n| n.child()))
            .or_else(|| node.downcast_ref::<gsk::DebugNode>().map(|n| n.child()));
    }
    match node(widget) {
        Some(_) => 1.0,
        None => 0.0,
    }
}

async fn notebook(app: &Rc<App>, rel: &str, said: &str) {
    app.open_path(rel);
    let mut tab = None;
    for _ in 0..200 {
        tab = app.doc_for(rel).and_then(|d| d.notebook().cloned());
        if tab.is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(5)).await;
    }
    let Some(tab) = tab else {
        return println!("bench motion no_tab {rel}");
    };
    glib::timeout_future(Duration::from_millis(800)).await;
    let [bar, .., head, gap] = tab.bench_motion_parts();
    tab.bench_hover(Some(1));
    say(said, "buttons_in", sampled(400, || drawn(&bar)).await);
    tab.bench_hover(Some(3));
    say(said, "buttons_moved", sampled(400, || drawn(&bar)).await);
    tab.bench_hover(None);
    // Moved once the event at hand is over (`NotebookTab::place_soon`).
    glib::timeout_future(Duration::from_millis(20)).await;
    let targets = bar.can_target();
    say(said, "buttons_out", sampled(400, || drawn(&bar)).await);
    println!(
        "bench motion {said} buttons_out targets={targets} shown={}",
        bar.is_visible()
    );

    head.set_state_flags(gtk::StateFlags::PRELIGHT, false);
    say(said, "strip_hover", sampled(400, || drawn(&head)).await);
    head.unset_state_flags(gtk::StateFlags::PRELIGHT);
    for n in [0, 2] {
        tab.bench_gap(n);
        say(said, "strip_gap", sampled(400, || drawn(&gap)).await);
    }
}
