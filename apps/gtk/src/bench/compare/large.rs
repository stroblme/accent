//! Measure main-loop stalls while opening, relayouting and editing a long comparison.

use super::*;

/// The other side of a comparison with `text`: lines changed at a step that gives a long file
/// about 1200 of them and a note one in seven, a tenth as many dropped and fewer added, `seed`
/// shifting which; with `every`, a character more on every line.
fn variant(text: &str, seed: usize, every: bool) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if every {
        let mark = ["x", "y", "z"][seed % 3];
        return lines.iter().map(|line| format!("{line}{mark}\n")).collect();
    }
    let step = (lines.len() / 1200).clamp(7, 97);
    let mut out = String::with_capacity(text.len() + text.len() / 50);
    for (i, line) in lines.iter().enumerate() {
        let i = i + seed;
        if i % (step * 10) == 3 {
            continue;
        }
        out.push_str(line);
        if i % step == 1 {
            out.push_str(" changed");
        }
        out.push('\n');
        if i % (step * 15) == 5 {
            out.push_str("an added line\n");
        }
    }
    out
}

/// Run `act` and wait until `compare` has laid what it asked for, with a tick asked of the main
/// loop every millisecond: how long `act` itself took, how long until the rows were laid, the
/// longest the main loop went without turning, from `act` until a while after, how long until the
/// rows on screen were level for good (`Compare::uneven`, looked at every 20 ms), and what the
/// relayout passes did since the last step (`Compare::passes`).
async fn held(compare: &diff::Compare, act: impl FnOnce()) -> (f64, f64, f64, f64, String) {
    let (worst, last) = (Rc::new(Cell::new(0.0)), Rc::new(Cell::new(Instant::now())));
    let beat = glib::timeout_add_local(Duration::from_millis(1), {
        let (worst, last) = (worst.clone(), last.clone());
        move || {
            let now = Instant::now();
            worst.set(f64::max(worst.get(), ms_since(last.replace(now))));
            glib::ControlFlow::Continue
        }
    });
    last.set(Instant::now());
    let t = Instant::now();
    act();
    let call = ms_since(t);
    // When rows on screen were last seen uneven.
    let mut level = 0.0;
    let mut look = || {
        if compare.uneven().is_some_and(|(n, _)| n > 0) {
            level = ms_since(t);
        }
    };
    // The editor re-reads a long text on its debounce, not on the keystroke.
    for step in 0..4513 {
        look();
        if step >= 13 && compare.settled() {
            break;
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    let settle = ms_since(t);
    for _ in 0..15 {
        look();
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    beat.remove();
    (call, settle, worst.get(), level, compare.passes())
}

/// Whether every cell of a CSV buffer carries its column's tag and nothing else carries one, as
/// a pass over all of it leaves them: what passes that re-tag only around an edit must keep.
fn csv_tagged(buffer: &sourceview5::Buffer) -> bool {
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    let offsets = crate::highlight::Offsets::new(&text);
    let mut wanted = vec![Vec::new(); crate::highlight::CSV_COLUMNS];
    for cell in accent_core::csv::columns(&text) {
        let range = (
            offsets.char_of(cell.range.start),
            offsets.char_of(cell.range.end),
        );
        if range.0 < range.1 {
            wanted[cell.column % crate::highlight::CSV_COLUMNS].push(range);
        }
    }
    wanted.iter().enumerate().all(|(column, wanted)| {
        let Some(tag) = buffer.tag_table().lookup(&format!("csv{column}")) else {
            return false;
        };
        let (mut spans, mut at) = (Vec::new(), buffer.start_iter());
        while at.has_tag(&tag) || at.forward_to_tag_toggle(Some(&tag)) {
            let from = at.offset();
            at.forward_to_tag_toggle(Some(&tag));
            spans.push((from, at.offset()));
        }
        spans == *wanted
    })
}

/// How many stretches of `buffer` carry the tag named `name`.
fn tagged(buffer: &sourceview5::Buffer, name: &str) -> usize {
    let Some(tag) = buffer.tag_table().lookup(name) else {
        return 0;
    };
    let (mut n, mut at) = (0, buffer.start_iter());
    while at.has_tag(&tag) || at.forward_to_tag_toggle(Some(&tag)) {
        n += 1;
        at.forward_to_tag_toggle(Some(&tag));
    }
    n
}

/// The median and the largest of `values`.
fn spread(values: &mut [f64]) -> (f64, f64) {
    values.sort_by(f64::total_cmp);
    let median = values.get(values.len() / 2).copied().unwrap_or(f64::NAN);
    (median, values.last().copied().unwrap_or(f64::NAN))
}

/// `ACCENT_BENCH_COMPARE=large:<rel>` compares a file with distributed changes;
/// `large:every:<rel>` changes each line. Relayout five times, type/undo three characters,
/// reread twice, resize 200 px each way and toggle unchanged lines.
///
/// Report `call_ms`, `settle_ms`, `held_ms`, `level_ms` (0 when never uneven), relayout passes
/// and emphasis spans; summarise median/worst stalls by step. CSV checks compare column tags
/// with a full pass. Restores and saves the file; use a scratch copy.
pub(in crate::bench) fn bench_compare_large(app: &Rc<App>, rel: &str) {
    let (every, rel) = match rel.strip_prefix("every:") {
        Some(rel) => (true, rel),
        None => (false, rel),
    };
    app.open_path(rel);
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let mut tab = None;
        for _ in 0..400 {
            tab = app.open_tabs().into_iter().find(|tab| tab.rel() == rel);
            if tab.as_ref().is_some_and(|tab| tab.buffer.char_count() > 0) {
                break;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        let Some(tab) = tab else {
            println!("bench compare_large no_tab");
            return bench_quit(&app);
        };
        // GTK measures a long text in idles of its own once it is shown.
        glib::timeout_future(Duration::from_millis(2000)).await;
        let text = tab.text();
        let lines = text.lines().count();
        let other = variant(&text, 0, every);
        let t = Instant::now();
        let compare = tab.compare(
            "Mine",
            ("Theirs", &other),
            diff::Side::New,
            true,
            None,
            "Large",
        );
        let call = ms_since(t);
        let (_, settle, worst, level, passes) = held(&compare, || {}).await;
        let (settle, worst) = (settle + call, worst.max(call));
        println!(
            "bench compare_large open lines={lines} call_ms={call:.1} settle_ms={settle:.0} held_ms={worst:.1} level_ms={level:.0} {} emphasised={} {passes}",
            bench_compare_line(&compare),
            tagged(&tab.buffer, "diff-added-emph")
        );
        let mut kinds: Vec<(&str, Vec<f64>)> = vec![("open", vec![worst])];
        let mut relay = Vec::new();
        for round in 0..5 {
            let (call, settle, worst, level, passes) = held(&compare, || compare.refresh()).await;
            println!(
                "bench compare_large relay round={round} call_ms={call:.1} settle_ms={settle:.0} held_ms={worst:.1} level_ms={level:.0} {passes}"
            );
            relay.push(worst);
        }
        kinds.push(("relay", relay));
        let mut typed = Vec::new();
        let at = compare
            .opens_at()
            .map(|o| tab.buffer.iter_at_offset(o).line())
            .unwrap_or(1);
        for round in 0..6 {
            let key = || {
                let mut end = tab.buffer.iter_at_line(at).unwrap_or(tab.buffer.end_iter());
                end.forward_to_line_end();
                if round % 2 == 0 {
                    tab.buffer.place_cursor(&end);
                    tab.buffer.insert_at_cursor("x");
                } else {
                    let mut start = end;
                    start.backward_char();
                    tab.buffer.delete(&mut start, &mut end);
                }
            };
            let (call, settle, worst, level, passes) = held(&compare, key).await;
            println!(
                "bench compare_large type round={round} call_ms={call:.1} settle_ms={settle:.0} held_ms={worst:.1} level_ms={level:.0} {passes}"
            );
            typed.push(worst);
        }
        kinds.push(("type", typed));
        if tab.flavour() == crate::editor::Flavour::Csv {
            println!("bench compare_large csv_tagged={}", csv_tagged(&tab.buffer));
        }
        let mut reread = Vec::new();
        for round in 1..3 {
            let other = variant(&text, round, every);
            let (call, settle, worst, level, passes) =
                held(&compare, || compare.set_side(diff::Side::Old, &other)).await;
            println!(
                "bench compare_large reread round={round} call_ms={call:.1} settle_ms={settle:.0} held_ms={worst:.1} level_ms={level:.0} {} {passes}",
                bench_compare_line(&compare)
            );
            reread.push(worst);
        }
        kinds.push(("reread", reread));
        let mut dragged = Vec::new();
        for by in [-200, 200] {
            let paned = compare.paned();
            let (call, settle, worst, level, passes) =
                held(&compare, || paned.set_position(paned.position() + by)).await;
            println!(
                "bench compare_large drag by={by} call_ms={call:.1} settle_ms={settle:.0} held_ms={worst:.1} level_ms={level:.0} {} {passes}",
                bench_compare_line(&compare)
            );
            dragged.push(worst);
        }
        kinds.push(("drag", dragged));
        let mut all = Vec::new();
        for on in [true, false] {
            let toggle = compare.unfold();
            let (call, settle, worst, level, passes) =
                held(&compare, || toggle.set_active(on)).await;
            println!(
                "bench compare_large all on={on} call_ms={call:.1} settle_ms={settle:.0} held_ms={worst:.1} level_ms={level:.0} {} {passes}",
                bench_compare_line(&compare)
            );
            all.push(worst);
        }
        kinds.push(("all", all));
        let summary: Vec<String> = kinds
            .iter_mut()
            .map(|(kind, held)| {
                let (median, worst) = spread(held);
                format!("{kind}={median:.1}/{worst:.1}")
            })
            .collect();
        println!(
            "bench compare_large held_ms median/worst {}",
            summary.join(" ")
        );
        bench_quit(&app);
    });
}
