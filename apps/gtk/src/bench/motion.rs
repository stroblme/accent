//! `ACCENT_BENCH_MOTION=<ipynb>,<folder>`: DESIGN.md's Motion, sampled every few milliseconds
//! with animations on and then off, the setting changed while the window runs, which every motion
//! has to follow at once. A line says whether a value between a motion's ends was seen (`mid`)
//! and where it ended.
//! - The notebook: the cell buttons coming to a cell and going, and the add strips.
//! - The Files tree's `<folder>`, holding a subfolder `d00` and a dot-named file: its arrow and the
//!   children it opens onto, and no arrow turning nor row fading while it scrolls or Show Hidden
//!   refills it.

use super::*;
use gtk::gsk;

pub(super) fn bench_motion(app: &Rc<App>, arg: &str) {
    let Some((ipynb, folder)) = arg.split_once(',') else {
        eprintln!("ACCENT_BENCH_MOTION=<ipynb>,<folder>");
        return bench_quit(app);
    };
    let (app, ipynb, folder) = (app.clone(), ipynb.to_string(), folder.to_string());
    glib::spawn_future_local(async move {
        for on in [true, false] {
            if let Some(settings) = gtk::Settings::default() {
                settings.set_gtk_enable_animations(on);
            }
            let said = if on { "on" } else { "off" };
            notebook(&app, &ipynb, said).await;
            tree(&app, &folder, said).await;
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

/// Whether `seen` held at some point over `ms`.
async fn ever(ms: u64, seen: impl Fn() -> bool) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(ms) {
        if seen() {
            return true;
        }
        glib::timeout_future(Duration::from_millis(4)).await;
    }
    false
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

/// The first node under `node` that `pick` answers for.
fn find<T>(node: gsk::RenderNode, pick: impl Fn(&gsk::RenderNode) -> Option<T>) -> Option<T> {
    let mut todo = vec![node];
    while let Some(node) = todo.pop() {
        if let Some(found) = pick(&node) {
            return Some(found);
        }
        if let Some(n) = node.downcast_ref::<gsk::ContainerNode>() {
            todo.extend((0..n.n_children()).map(|i| n.child(i)));
        }
        let child = (node.downcast_ref::<gsk::TransformNode>().map(|n| n.child()))
            .or_else(|| node.downcast_ref::<gsk::OpacityNode>().map(|n| n.child()))
            .or_else(|| node.downcast_ref::<gsk::ClipNode>().map(|n| n.child()))
            .or_else(|| {
                node.downcast_ref::<gsk::RoundedClipNode>()
                    .map(|n| n.child())
            })
            .or_else(|| node.downcast_ref::<gsk::DebugNode>().map(|n| n.child()));
        todo.extend(child);
    }
    None
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

/// How far `widget`'s drawing is turned, in degrees.
fn turned(widget: &impl IsA<gtk::Widget>) -> f64 {
    let angle = |n: &gsk::RenderNode| {
        let angle = n
            .downcast_ref::<gsk::TransformNode>()?
            .transform()
            .to_2d_components()
            .4;
        (angle.abs() > 0.01).then_some(angle)
    };
    node(widget)
        .and_then(|node| find(node, angle))
        .map_or(0.0, f64::from)
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

/// `folder`'s arrow and children as it opens, then its rows as it scrolls and refills.
async fn tree(app: &Rc<App>, folder: &str, said: &str) {
    let Some(tree) = app.tree.get() else {
        return println!("bench motion no_tree");
    };
    let mut row = None;
    for _ in 0..100 {
        row = tree::find_row(tree.model(), folder);
        if row.is_some() {
            break;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    let Some(row) = row else {
        return println!("bench motion no_row {folder}");
    };
    row.set_expanded(false);
    glib::timeout_future(Duration::from_millis(600)).await;
    // The arrow is the expander's own child, apart from the row's icon and name.
    let arrow = tree::expanders(tree.view())
        .into_iter()
        .find(|e| e.list_row().as_ref() == Some(&row))
        .and_then(|e| e.first_child())
        .filter(|w| w.css_name() == "expander");
    let Some(arrow) = arrow else {
        return println!("bench motion no_arrow {folder}");
    };
    let opened = tree.model().n_items();
    row.set_expanded(true);
    let rows = || tree::expanders(tree.view());
    let least = || rows().iter().map(|e| e.opacity()).fold(1.0, f64::min);
    let [turn, children] = sampled_all(400, || [turned(&arrow) / -90.0, least()]).await;
    say(said, "arrow_open", turn);
    say(said, "children_in", children);
    println!(
        "bench motion {said} children landed={}",
        tree.model().n_items() > opened
    );
    row.set_expanded(false);
    say(
        said,
        "arrow_close",
        sampled(400, || turned(&arrow) / -90.0).await,
    );

    // A long folder with one open inside: its rows' widgets are bound to rows open and closed.
    row.set_expanded(true);
    if let Some(inner) = tree::find_row(tree.model(), &format!("{folder}/d00")) {
        inner.set_expanded(true);
    }
    glib::timeout_future(Duration::from_millis(800)).await;
    let (mut spun, mut faded, mut seen) = (0, 0, 0);
    let scroll = tree.view().parent().and_downcast::<gtk::ScrolledWindow>();
    if let Some(adj) = scroll.map(|s| s.vadjustment()) {
        let end = adj.upper() - adj.page_size();
        for i in (0..=60).chain((0..60).rev()) {
            adj.set_value(end * f64::from(i) / 60.0);
            glib::timeout_future(Duration::from_millis(12)).await;
            for e in rows() {
                let angle = e.first_child().map_or(0.0, |a| turned(&a)).abs();
                spun += usize::from(angle > 1.0 && angle < 89.0);
                faded += usize::from(e.opacity() < 0.98);
                seen += 1;
            }
        }
    }
    println!("bench motion {said} scrolled rows={seen} spun={spun} faded={faded}");
    tree.set_show_hidden(true);
    let faded = ever(600, || least() < 0.98).await;
    println!("bench motion {said} refill faded={faded}");
    tree.set_show_hidden(false);
    row.set_expanded(false);
}
