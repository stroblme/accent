//! Typing into a note with the preview beside it: what each re-render costs and what it keeps.

use super::*;
use accent_core::search::Options;
use webkit6::prelude::*;

/// Keys typed for the cost, at the 2.5 a second of steady typing: each one past the 300 ms
/// debounce, so each a re-render of its own.
const KEYS: usize = 12;
const EVERY: Duration = Duration::from_millis(400);

/// `ACCENT_BENCH_PREVIEW_LOOK=type:<rel_note>[,<word>]` puts the caret at the end of a paragraph
/// halfway down the note, with the preview beside it, and prints:
///
/// - `cost`: [`KEYS`] keys typed there, what the web process spent on them (`web_cpu_ms`, all its
///   threads, per render too) and how long each render took from being asked for to its page
///   being up (`latency_ms`, median and worst); `shown` whether the page holds what was typed.
/// - `select`: a paragraph below the caret selected, one more key typed, and the page either side
///   (scroll, selection, whether it is still one, diagrams drawn, fences still to draw, diagrams
///   kept from before), with the least scroll seen while the render landed (`min_scroll`): 0 is
///   a page thrown away.
/// - `find`: `<word>` (default `readout`) searched for and stepped to its third match, one more
///   key typed, and the readout and the selection either side.
/// - `lines`: lines added at the caret, and whether the page's source-line markers are then
///   those of a fresh render.
/// - `lost`: WebKit's process killed under the page and a key typed, and whether the page then
///   shows it.
///
/// It types into the note, so a scratch vault only.
pub(super) fn bench_preview_type(app: &Rc<App>, arg: &str) {
    scratch_only(app, "ACCENT_BENCH_PREVIEW_LOOK=type:");
    let (rel, word) = arg.split_once(',').unwrap_or((arg, "readout"));
    app.open_path(rel);
    let (app, word) = (app.clone(), word.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        app.set_mode(Mode::Split);
        let (Some(tab), Some(view)) = (
            app.active(),
            app.preview.borrow().as_ref().map(|p| p.view().clone()),
        ) else {
            println!("bench preview_type no_preview");
            return bench_quit(&app);
        };
        settle(&app).await;
        let lines = tab.buffer.line_count();
        for line in lines / 2..lines {
            let Some(start) = tab.buffer.iter_at_line(line) else {
                continue;
            };
            let mut end = start;
            end.forward_to_line_end();
            let text = tab.buffer.text(&start, &end, false);
            if text.len() > 40 && text.starts_with(|c: char| c.is_ascii_uppercase()) {
                tab.buffer.place_cursor(&end);
                break;
            }
        }
        // Until the web process is done with the page, its images decoded: half a second it
        // spends under 20 ms in.
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(20) {
            let before = web_cpu_ms();
            glib::timeout_future(Duration::from_millis(500)).await;
            if web_cpu_ms() - before < 20 {
                break;
            }
        }
        let (web, mut latency) = (web_cpu_ms(), Vec::with_capacity(KEYS));
        for ch in " preview".chars().cycle().take(KEYS) {
            let key = Instant::now();
            tab.buffer.insert_at_cursor(&ch.to_string());
            latency.push(render(&app, None).await);
            if let Some(rest) = EVERY.checked_sub(key.elapsed()) {
                glib::timeout_future(rest).await;
            }
        }
        glib::timeout_future(Duration::from_millis(500)).await;
        let web = web_cpu_ms() - web;
        latency.sort_by(f64::total_cmp);
        let shown = js(&view, "document.body.textContent.includes(' preview pre')").await;
        println!(
            "bench preview_type cost bytes={} keys={KEYS} web_cpu_ms={web} per_render={} \
             latency_ms={:.0}/{:.0} shown={shown}",
            tab.text().len(),
            web / KEYS as u64,
            latency[KEYS / 2],
            latency[KEYS - 1],
        );

        let select = format!(
            "var marks = Array.from(document.querySelectorAll('p > span[data-line]')); \
             var at = marks.findIndex(function (m) {{ \
               return +m.getAttribute('data-line') > {}; }}); \
             var r = document.createRange(); r.selectNodeContents(marks[at + 1].parentElement); \
             getSelection().removeAllRanges(); getSelection().addRange(r);",
            tab.cursor_line()
        );
        js(&view, &select).await;
        let before = js(&view, STATE).await;
        tab.buffer.insert_at_cursor("s");
        let mut least = f64::MAX;
        render(&app, Some((&view, &mut least))).await;
        glib::timeout_future(Duration::from_millis(500)).await;
        println!(
            "bench preview_type select before={before} after={} min_scroll={least}",
            js(&view, STATE).await
        );

        let readout = Rc::new(RefCell::new(String::new()));
        if let Some(preview) = app.preview.borrow().as_ref() {
            preview.connect_found(glib::clone!(
                #[strong]
                readout,
                move |label| *readout.borrow_mut() = label.to_string()
            ));
            preview.find(&word, Options::default());
        }
        glib::timeout_future(Duration::from_millis(500)).await;
        for _ in 0..2 {
            if let Some(preview) = app.preview.borrow().as_ref() {
                preview.find_next();
            }
            glib::timeout_future(Duration::from_millis(200)).await;
        }
        let said = readout.borrow().clone();
        let before = format!("{said:?} {}", js(&view, STATE).await);
        tab.buffer.insert_at_cursor("s");
        render(&app, None).await;
        glib::timeout_future(Duration::from_millis(500)).await;
        let state = js(&view, STATE).await;
        println!(
            "bench preview_type find before={before} after={:?} {state}",
            readout.borrow()
        );

        // Lines added above the blocks below the caret: the page's markers are a fresh render's.
        tab.buffer.insert_at_cursor("\n\nAdded.\n");
        render(&app, None).await;
        let page = js(
            &view,
            "Array.from(document.querySelectorAll('[data-line]')) \
             .map(function (m) { return m.getAttribute('data-line'); }).join(' ')",
        )
        .await;
        let html = accent_core::markdown::to_html(&tab.text());
        let fresh: Vec<&str> = html
            .split("data-line=\"")
            .skip(1)
            .filter_map(|s| s.split('"').next())
            .collect();
        println!(
            "bench preview_type lines markers={} match={}",
            fresh.len(),
            page == fresh.join(" ")
        );

        // WebKit's process killed under the page, which the app renders again in a new one, and
        // a key typed after: a patch of the page that went with it would show nothing.
        for (pid, _) in memory::descendants()
            .into_iter()
            .filter(|(_, name)| name.starts_with("WebKitWebProc"))
        {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
        glib::timeout_future(Duration::from_secs(2)).await;
        tab.buffer.insert_at_cursor("Back.");
        render(&app, None).await;
        println!(
            "bench preview_type lost shown={}",
            js(&view, "document.body.textContent.includes('Back.')").await
        );
        bench_quit(&app);
    });
}

/// The page as the reader has it: how far down it is scrolled, the start of the selection and
/// whether there is one, the diagrams drawn, the fences still to draw, and how many diagrams are
/// the ones the last call saw, which it marks.
const STATE: &str = "var s = getSelection(); \
    var d = Array.from(document.querySelectorAll('pre.mermaid')); \
    var kept = d.filter(function (p) { return p.hasAttribute('data-seen'); }).length; \
    d.forEach(function (p) { p.setAttribute('data-seen', ''); }); \
    JSON.stringify([scrollY, String(s).slice(0, 30), s.rangeCount > 0 && !s.isCollapsed, \
    document.querySelectorAll('pre.mermaid svg').length, \
    document.querySelectorAll('code.language-mermaid').length, kept])";

/// Whether the preview's page is this render's, or the last one's once it landed.
fn settled(app: &Rc<App>) -> bool {
    app.preview.borrow().as_ref().is_some_and(|p| p.settled())
}

/// Wait for the page the preview is loading, and a second for its diagrams.
async fn settle(app: &Rc<App>) {
    let t = Instant::now();
    while !settled(app) && t.elapsed() < Duration::from_secs(20) {
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    glib::timeout_future(Duration::from_secs(1)).await;
}

/// Wait for the render a key asks for once the debounce runs out, and say how long it took from
/// being asked for to its page being up. With `scroll`, the least scroll the page showed meanwhile.
async fn render(app: &Rc<App>, mut scroll: Option<(&webkit6::WebView, &mut f64)>) -> f64 {
    let key = Instant::now();
    while settled(app) && key.elapsed() < Duration::from_secs(2) {
        glib::timeout_future(Duration::from_millis(2)).await;
    }
    let asked = Instant::now();
    while !settled(app) && asked.elapsed() < Duration::from_secs(20) {
        if let Some((view, least)) = scroll.as_mut() {
            let y: f64 = js(view, "scrollY").await.parse().unwrap_or(f64::MAX);
            **least = least.min(y);
        }
        glib::timeout_future(Duration::from_millis(2)).await;
    }
    ms_since(asked)
}

async fn js(view: &webkit6::WebView, script: &str) -> String {
    view.evaluate_javascript_future(script, None, None)
        .await
        .map_or_else(|e| e.to_string(), |v| v.to_str().to_string())
}

/// CPU the app's WebKit web processes have used, every thread, in ms: `/proc`'s clock ticks are
/// `USER_HZ`, 100 a second on Linux.
fn web_cpu_ms() -> u64 {
    memory::descendants()
        .into_iter()
        .filter(|(_, name)| name.starts_with("WebKitWebProc"))
        .filter_map(|(pid, _)| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let rest = stat.rsplit_once(") ")?.1;
            let mut fields = rest.split(' ').skip(11);
            let user: u64 = fields.next()?.parse().ok()?;
            let system: u64 = fields.next()?.parse().ok()?;
            Some((user + system) * 10)
        })
        .sum()
}
