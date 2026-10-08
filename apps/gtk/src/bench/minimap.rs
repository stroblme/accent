//! The minimap's drills: whether the band covers what the view shows, where a press and a drag
//! leave the view, and what the map costs a scrolling frame.

use super::*;
use crate::minimap::Minimap;
use accent_core::config::Theme;

/// How many steps a `frames` or `typing` round takes.
const STEPS: usize = 120;
/// How long a frame or two takes to land.
const FRAME: Duration = Duration::from_millis(300);
/// How long a round waits for the XTEST input it asked for.
const XTEST: Duration = Duration::from_secs(10);

/// `ACCENT_BENCH_MINIMAP=<round>:<rel>` fills the note at `rel` with 256 KB of sections, switches
/// the minimap on and prints one round's lines, so point it at a scratch vault.
///
/// - `band` prints the lines the view shows beside the lines under the band's edges, and the
///   band's pixels in the map (`view=10..52 band=Some((10, 52)) px=…/900 ok=true`): with the view
///   at its top, a quarter, half way and at its end, with the first section folded, unwrapped
///   (Alt+Z), zoomed to 150 %, and over a comparison with the disk with its unchanged runs
///   collapsed and with Show All Unchanged Lines down. `ok` is both pairs equal and the band
///   inside the map.
/// - `jump` presses the map at 70 % of its height through the widget's own press and prints the
///   line pressed and the lines on screen after it, the moment it lands and a second later once
///   GTK has laid out the lines above (`ok`: the line in the middle third, or on screen with the
///   view at its end); then prints `aim <x> <y>` at 30 % for `build-aux/xtest.py :N "move <x> <y>;
///   down; up"` and the same once the press has scrolled the view.
/// - `drag` takes hold of the band through the widget's press and drags it 40, 120 and -60 px,
///   printing the band's top before and after each (`ok`: it moved with the pointer, within a
///   row), then from the top of the note 80 % of the map's height down, and a second later;
///   then prints `aim <x> <y> <dy>` for `"move <x> <y>; down; move <x> <y+dy/2>; move <x>
///   <y+dy>; up"` and the same once that drag has scrolled the view.
/// - `look` saves the window as `minimap-<theme>-<what>.png` in the working directory, light and
///   dark: a note holding h1 to h4 and a setext heading, at its top and a quarter of the way down,
///   a comparison with the disk where it opens, on its first hunk, and a code file it writes
///   beside the note.
/// - `frames` scrolls the note down by a quarter page [`STEPS`] times, a step per frame, with the
///   map off and then on, three times over as the machine's load moves, printing each round's
///   median and worst frame: `paint_ms` from the end of the layout phase to the end of painting
///   (the snapshot and the render), `frame_ms` from the frame's start to the end of painting, and
///   `paint_cpu_ms` the main thread's CPU time over the painting, which a busy machine does not
///   inflate. `typing` is the same with a character typed into the middle of the note every
///   150 ms instead, adding the main thread's CPU time over the round (`cpu_ms`) and the median
///   keystroke's own time (`key_us`), as `ACCENT_BENCH_STYLE=typing:` prints them. Both print
///   what the map's own drawing cost per step (`map_us`).
pub(super) fn bench_minimap(app: &Rc<App>, arg: &str) {
    scratch_only(app, "ACCENT_BENCH_MINIMAP");
    let Some((round, rel)) = arg.split_once(':') else {
        eprintln!("ACCENT_BENCH_MINIMAP=<band|jump|drag|look|frames|typing>:<rel>");
        return bench_quit(app);
    };
    app.open_path(rel);
    let (app, round, rel) = (app.clone(), round.to_string(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        let Some(tab) = app.open_tabs().into_iter().find(|tab| tab.rel() == rel) else {
            return bench_quit(&app);
        };
        let section = super::style::TYPING_SECTION;
        tab.set_text(&section.repeat(256 * 1024 / section.len()));
        tab.set_minimap(!matches!(round.as_str(), "frames" | "typing"));
        glib::timeout_future(Duration::from_secs(1)).await;
        let map = tab.minimap().downcast_ref::<Minimap>().cloned();
        match (round.as_str(), map) {
            ("band", Some(map)) => band(&app, &tab, &map).await,
            ("jump", Some(map)) => jump(&app, &tab, &map).await,
            ("drag", Some(map)) => drag(&app, &tab, &map).await,
            ("look", Some(_)) => look(&app, &tab).await,
            (round @ ("frames" | "typing"), _) => {
                let step = match round {
                    "frames" => Step::Scroll,
                    _ => Step::Key,
                };
                for on in [false, true, false, true, false, true] {
                    tab.set_minimap(on);
                    frames(&app, &tab, step, if on { "on" } else { "off" }).await;
                }
            }
            _ => eprintln!("ACCENT_BENCH_MINIMAP: no round {round:?}"),
        }
        bench_quit(&app);
    });
}

/// Scroll `tab` to `frac` of the way down, and let it draw.
async fn scroll(tab: &Tab, frac: f64) {
    if let Some(adj) = tab.view.vadjustment() {
        adj.set_value(frac * (adj.upper() - adj.page_size()));
    }
    glib::timeout_future(FRAME).await;
}

/// The view's first and last lines on screen.
fn shown(tab: &Tab) -> (usize, usize) {
    let rect = tab.view.visible_rect();
    let line = |y: i32| tab.view.line_at_y(y).0.line().max(0) as usize;
    (line(rect.y()), line(rect.y() + rect.height() - 1))
}

fn say_band(tab: &Tab, map: &Minimap, at: &str) {
    let (first, last) = shown(tab);
    let band = map.band_lines();
    let (y, h) = map.band_px().unwrap_or((f64::NAN, f64::NAN));
    let inside = y >= -0.5 && y + h <= f64::from(map.height()) + 0.5;
    println!(
        "bench minimap band at={at} view={first}..{last} band={band:?} px={y:.0}+{h:.0}/{} \
         ok={}",
        map.height(),
        band == Some((first, last)) && inside
    );
}

async fn band(app: &Rc<App>, tab: &Rc<Tab>, map: &Minimap) {
    for (at, frac) in [("top", 0.0), ("quarter", 0.25), ("half", 0.5), ("end", 1.0)] {
        scroll(tab, frac).await;
        say_band(tab, map, at);
    }
    scroll(tab, 0.0).await;
    tab.toggle_fold(0);
    glib::timeout_future(FRAME).await;
    say_band(tab, map, "folded");
    tab.toggle_fold(0);
    tab.toggle_wrap();
    scroll(tab, 0.5).await;
    say_band(tab, map, "unwrapped");
    tab.toggle_wrap();
    tab.set_font(None, 1.5);
    glib::timeout_future(FRAME).await;
    scroll(tab, 0.5).await;
    say_band(tab, map, "zoomed");
    tab.set_font(None, 1.0);

    let Some(compare) = compared(app, tab).await else {
        println!("bench minimap band compare=none");
        return;
    };
    for (at, frac) in [("compare_top", 0.0), ("compare_half", 0.5)] {
        scroll(tab, frac).await;
        say_band(tab, map, at);
    }
    let all = find_widget(compare.widget(), &|w| {
        w.is::<gtk::ToggleButton>()
            && w.tooltip_text().as_deref() == Some("Show All Unchanged Lines")
    });
    match all.and_downcast::<gtk::ToggleButton>() {
        Some(all) => {
            all.set_active(true);
            glib::timeout_future(Duration::from_secs(1)).await;
            scroll(tab, 0.5).await;
            say_band(tab, map, "compare_all");
        }
        None => println!("bench minimap band show_all=none"),
    }
    tab.leave_compare();
}

/// The note compared with its disk copy, every fortieth of its 1200 lines changed, once the
/// comparison has settled.
async fn compared(app: &Rc<App>, tab: &Rc<Tab>) -> Option<Rc<diff::Compare>> {
    let text = |changed: bool| -> String {
        (1..=1200)
            .map(|i| match changed && i % 40 == 20 {
                true => format!("line {i} changed\n"),
                false => format!("line {i} {}\n", "wrapping words ".repeat(i % 7 * 5)),
            })
            .collect()
    };
    tab.set_text(&text(false));
    app.write_tab(tab, None).ok()?;
    tab.set_text(&text(true));
    app.compare_with_disk(tab);
    for _ in 0..200 {
        if let Some(compare) = tab.comparison().filter(|c| c.settled()) {
            return Some(compare);
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    None
}

async fn look(app: &Rc<App>, tab: &Rc<Tab>) {
    let note: String = (0..40)
        .map(|i| {
            let chapter = match i % 8 {
                0 => format!("# Chapter {}\n\n", i / 8 + 1),
                _ => String::new(),
            };
            let section = super::style::TYPING_SECTION.replacen(
                "## Section",
                &format!("## Section {}", i + 1),
                1,
            );
            let deeper = format!(
                "### Part {}.1\n\nA few words.\n\n#### Detail\n\nMore.\n\n",
                i + 1
            );
            chapter + &section + &deeper
        })
        .collect();
    let note = format!("Setext title\n============\n\n{note}");
    let shoot = |what: &str, theme: Theme| {
        let name = format!("minimap-{}-{what}.png", format!("{theme:?}").to_lowercase());
        let saved = window_png(&app.window, Path::new(&name));
        println!("bench minimap look {name} saved={saved}");
    };
    for theme in [Theme::Light, Theme::Dark] {
        crate::theme::apply(theme);
        tab.set_text(&note);
        glib::timeout_future(Duration::from_secs(1)).await;
        scroll(tab, 0.0).await;
        shoot("note-top", theme);
        scroll(tab, 0.25).await;
        shoot("note", theme);
        if compared(app, tab).await.is_some() {
            glib::timeout_future(FRAME).await;
            shoot("compare", theme);
        }
        tab.leave_compare();
    }
    // A code file half way down, whose keywords and comments in the map's lines off the view are
    // toned too.
    let code: String = (0..60)
        .map(|i| {
            format!(
                "/// What step {i} does,\n/// in two lines.\npub fn step_{i}(x: u32) -> u32 \
                 {{\n    // add\n    let y = x + {i};\n    if y > 10 {{\n        return y;\n    \
                 }}\n    y * 2\n}}\n\n"
            )
        })
        .collect();
    if std::fs::write(app.root().join("minimap-look.rs"), code).is_ok() {
        app.open_path("minimap-look.rs");
        glib::timeout_future(Duration::from_secs(1)).await;
        if let Some(code) = app
            .open_tabs()
            .into_iter()
            .find(|t| t.rel() == "minimap-look.rs")
        {
            code.set_minimap(true);
            scroll(&code, 0.5).await;
            glib::timeout_future(FRAME).await;
            for theme in [Theme::Light, Theme::Dark] {
                crate::theme::apply(theme);
                glib::timeout_future(Duration::from_secs(1)).await;
                shoot("code", theme);
            }
        }
    }
}

/// `window` as it is on screen, painted by its own renderer into `path`.
fn window_png(window: &adw::ApplicationWindow, path: &Path) -> bool {
    let (w, h) = (f64::from(window.width()), f64::from(window.height()));
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(&snapshot, w, h);
    let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) else {
        return false;
    };
    let shown = graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
    renderer
        .render_texture(&node, Some(&shown))
        .save_to_png(path)
        .is_ok()
}

/// Where on the window `y` of the map is, for XTEST: under Xvfb with no window manager the
/// window sits at 0,0.
fn aim(app: &Rc<App>, map: &Minimap, y: f64) -> Option<(f32, f32)> {
    let point = graphene::Point::new(map.width() as f32 / 2.0, y as f32);
    let at = map.compute_point(&app.window, &point)?;
    Some((at.x(), at.y()))
}

/// Wait for the view to scroll, and a moment more for it to settle.
async fn moved(tab: &Tab) -> bool {
    let Some(adj) = tab.view.vadjustment() else {
        return false;
    };
    let from = adj.value();
    let start = Instant::now();
    while start.elapsed() < XTEST {
        glib::timeout_future(Duration::from_millis(50)).await;
        if adj.value() != from {
            glib::timeout_future(Duration::from_millis(500)).await;
            return true;
        }
    }
    false
}

fn say_jump(tab: &Tab, how: &str, pressed: Option<usize>) {
    let (first, last) = shown(tab);
    let third = (last - first) / 3;
    let end = tab.buffer.line_count().max(1) as usize - 1;
    let ok = pressed.is_some_and(|line| {
        (first + third..=last - third).contains(&line) || (last >= end && line >= first)
    });
    println!("bench minimap jump how={how} pressed={pressed:?} view={first}..{last} ok={ok}");
}

async fn jump(app: &Rc<App>, tab: &Rc<Tab>, map: &Minimap) {
    scroll(tab, 0.0).await;
    let y = f64::from(map.height()) * 0.7;
    let pressed = map.line_at(y);
    map.press_at(y);
    map.let_go();
    glib::timeout_future(FRAME).await;
    say_jump(tab, "widget", pressed);
    glib::timeout_future(Duration::from_secs(1)).await;
    say_jump(tab, "widget_settled", pressed);

    scroll(tab, 0.0).await;
    let y = f64::from(map.height()) * 0.3;
    let pressed = map.line_at(y);
    if let Some((x, y)) = aim(app, map, y) {
        println!("bench minimap jump aim {x:.0} {y:.0}");
    }
    match moved(tab).await {
        true => say_jump(tab, "xtest", pressed),
        false => println!("bench minimap jump how=xtest none"),
    }
}

fn say_drag(how: &str, dy: f64, before: f64, after: f64) {
    let ok = (after - before - dy).abs() <= 2.0;
    println!("bench minimap drag how={how} dy={dy:.0} top={before:.1}->{after:.1} ok={ok}");
}

async fn drag(app: &Rc<App>, tab: &Rc<Tab>, map: &Minimap) {
    scroll(tab, 0.3).await;
    let top = || map.band_px().map_or(f64::NAN, |(y, _)| y);
    let (y, h) = map.band_px().unwrap_or_default();
    let start = top();
    map.press_at(y + h / 2.0);
    for dy in [40.0, 120.0, -60.0] {
        map.drag_by(dy);
        glib::timeout_future(FRAME).await;
        say_drag("widget", dy, start, top());
    }
    map.let_go();
    // From the top to most of the way down the map in one move: lines GTK has not laid out yet,
    // where it has only estimated their heights, and the same a second later once it has.
    scroll(tab, 0.0).await;
    let (y, h) = map.band_px().unwrap_or_default();
    let (start, dy) = (top(), f64::from(map.height()) * 0.8);
    map.press_at(y + h / 2.0);
    // In steps with a frame between them, as a pointer moves: each is measured against the band
    // as the last frame drew it.
    for step in 1..=8 {
        map.drag_by(dy * f64::from(step) / 8.0);
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    map.let_go();
    glib::timeout_future(FRAME).await;
    say_drag("far", dy, start, top());
    glib::timeout_future(Duration::from_secs(1)).await;
    say_drag("far_settled", dy, start, top());

    scroll(tab, 0.3).await;
    let (y, h) = map.band_px().unwrap_or_default();
    let start = top();
    let dy = 100.0;
    if let Some((x, y)) = aim(app, map, y + h / 2.0) {
        println!("bench minimap drag aim {x:.0} {y:.0} {dy:.0}");
    }
    match moved(tab).await {
        true => {
            glib::timeout_future(Duration::from_secs(1)).await;
            say_drag("xtest", dy, start, top());
        }
        false => println!("bench minimap drag how=xtest none"),
    }
}

/// What each step of a [`frames`] round does.
#[derive(Clone, Copy, PartialEq)]
enum Step {
    /// A quarter of a page down, the next as soon as it is painted, from the top of the note.
    Scroll,
    /// A character typed into the middle of the note, the next 150 ms later.
    Key,
}

/// One round of [`bench_minimap`]'s `frames` or `typing`, [`STEPS`] steps long.
async fn frames(app: &Rc<App>, tab: &Rc<Tab>, step: Step, map: &str) {
    let Some(adj) = tab.view.vadjustment() else {
        return;
    };
    match step {
        Step::Scroll => adj.set_value(0.0),
        Step::Key => {
            if let Some(at) = tab.buffer.iter_at_line(tab.buffer.line_count() / 2) {
                tab.buffer.place_cursor(&at);
            }
            tab.view
                .scroll_to_mark(&tab.buffer.get_insert(), 0.0, true, 0.0, 0.5);
        }
    }
    glib::timeout_future(Duration::from_secs(2)).await;
    let Some(clock) = app.window.frame_clock() else {
        return;
    };
    // The main thread's CPU time, which another process taking the core does not inflate.
    let cpu = || {
        std::fs::read_to_string("/proc/thread-self/schedstat")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0)
    };
    // Connected after GTK's own handlers: a before-paint's mark is where the frame starts, a
    // layout's where its painting starts, and a paint's where that ends.
    let marks = Rc::new(RefCell::new(Vec::new()));
    let mark = |phase: char| {
        let marks = marks.clone();
        move |_: &gdk::FrameClock| marks.borrow_mut().push((phase, Instant::now(), cpu()))
    };
    let handlers = [
        clock.connect_before_paint(mark('b')),
        clock.connect_layout(mark('l')),
        clock.connect_paint(mark('p')),
    ];
    let painted = || marks.borrow().iter().filter(|m| m.0 == 'p').count();
    let minimap = tab.minimap().downcast_ref::<Minimap>().cloned();
    let spent = || minimap.as_ref().map_or(Duration::ZERO, Minimap::take_spent);
    spent();
    let (start, mut keys) = (cpu(), Vec::with_capacity(STEPS));
    for ch in super::style::TYPING_WORDS.chars().cycle().take(STEPS) {
        let before = painted();
        match step {
            Step::Scroll => adj.set_value(adj.value() + adj.page_size() / 4.0),
            Step::Key => {
                let key = Instant::now();
                tab.buffer.insert_at_cursor(&ch.to_string());
                keys.push(key.elapsed().as_micros());
                glib::timeout_future(Duration::from_millis(150)).await;
                continue;
            }
        }
        for _ in 0..100 {
            glib::timeout_future(Duration::from_millis(2)).await;
            if painted() > before {
                break;
            }
        }
    }
    // The debounced restyle after the last key, and the frame it leaves.
    if step == Step::Key {
        glib::timeout_future(Duration::from_millis(500)).await;
    }
    let used = (cpu() - start) / 1_000_000;
    let map_us = spent().as_micros() / STEPS as u128;
    for handler in handlers {
        clock.disconnect(handler);
    }
    keys.sort_unstable();
    let marks = marks.take();
    let between = |from: char, to: char, on_cpu: bool| -> Vec<f64> {
        let mut spans: Vec<f64> = marks
            .iter()
            .enumerate()
            .filter(|(_, m)| m.0 == from)
            .filter_map(|(at, m)| {
                let end = marks[at..].iter().find(|e| e.0 == to)?;
                Some(match on_cpu {
                    true => (end.2 - m.2) as f64 / 1e6,
                    false => (end.1 - m.1).as_secs_f64() * 1000.0,
                })
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
    let round = match step {
        Step::Scroll => "frames",
        Step::Key => "typing",
    };
    println!(
        "bench minimap {round} map={map} steps={STEPS} frames={} paint_ms={} frame_ms={} \
         paint_cpu_ms={} cpu_ms={used} key_us={} map_us={map_us}",
        marks.iter().filter(|m| m.0 == 'p').count(),
        say(&between('l', 'p', false)),
        say(&between('b', 'p', false)),
        say(&between('l', 'p', true)),
        keys.get(keys.len() / 2).copied().unwrap_or(0),
    );
}
