//! PDF drawing drills for the pen, its ring, New Drawing and Insert Sketch.

use super::pdf::{online, opened, render_cpu_ms, vault_pages, written};
use super::*;

/// The drawing ring with the pen in hand: the tool and options it shows while the pen's eraser
/// end is at the page and once its tip is back, told by hand since Xvfb has no stylus, the
/// window's tool and the config untouched; then under Light, Dark and Solarized the swatches as
/// painted and the pen's colour as written, with a picture of the ring for each in `$TMPDIR`.
pub(super) fn bench_pdf_ring(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench ring no_tab");
            return bench_quit(&app);
        };
        app.pdf_mode(pdfview::Mode::Pen);
        let view = pdf.views()[0].clone();
        let Some(reader) = view.downcast_ref::<pdfview::PdfView>() else {
            return bench_quit(&app);
        };
        let config = app.config.borrow().drawing.clone();
        for eraser in [true, false] {
            reader.bench_pen(eraser);
            println!(
                "bench ring eraser={eraser} {} tool={:?} mode={:?} config_kept={}",
                pdf.ring().state(),
                app.tool.get(),
                pdf.mode(),
                app.config.borrow().drawing == config
            );
        }
        // The mouse moved over the page after the eraser end is another hand: the pen comes back.
        reader.bench_pen(true);
        let (sx, sy) = app.window.surface_transform();
        let mut steps = Vec::new();
        for f in [0.4, 0.5] {
            let p = graphene::Point::new(view.width() as f32 * f, view.height() as f32 * f);
            let p = view.compute_point(&app.window, &p).unwrap_or(p);
            steps.push(format!(
                "move {:.0} {:.0}",
                f64::from(p.x()) + sx,
                f64::from(p.y()) + sy
            ));
        }
        super::git::xtest(&steps.join("; ")).await;
        println!("bench ring mouse {}", pdf.ring().state());
        let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        use accent_core::config::Theme;
        for (name, theme) in [
            ("light", Theme::Light),
            ("dark", Theme::Dark),
            ("solarized", Theme::Solarized),
        ] {
            crate::theme::apply(theme);
            glib::timeout_future(Duration::from_millis(300)).await;
            pdf.restyle();
            style::bench_painted(&app.window, String::new).await;
            let path = std::path::Path::new(&dir).join(format!("ring-{name}.png"));
            let shot = super::diagram::shoot(pdf.ring().widget(), &path);
            println!(
                "bench ring theme={name} {} written={:?} pictured={shot}",
                pdf.ring().state(),
                reader.ink_style(pdfview::Mode::Pen).rgba,
            );
        }
        bench_quit(&app);
    });
}

/// The pen drawing 24 strokes on `rel` through XTEST, a motion every 10 ms, while the PDF text pass
/// reads what the vault holds unread and each save of `rel` again (with `,stop` it is stopped
/// first): how long the reading view went without a frame, and without a motion while the button
/// was down, and how many strokes reached the file with how many points each. A gap past 50 ms is
/// a stroke drawn straight. With `,<n>`, the first page gets `n` strokes down its left edge first.
///
/// Then what the render thread did per stroke — its CPU time, and how many strokes it listed for
/// the tools — and whether the strokes the view holds for the first page agree with the file's,
/// under the same names a fresh list gives them, after the strokes, an eraser drag down the
/// middle, its Undo, a Select drag of the first stroke, a quarter turn of it by its ring with its
/// Undo and Redo, and the file broken under the tab and mended.
pub(super) fn bench_pdf_pen(app: &Rc<App>, arg: &str) {
    let mut parts = arg.split(',');
    let rel = parts.next().unwrap_or_default().to_string();
    let (mut stop, mut fill) = (false, 0);
    for part in parts {
        match part {
            "stop" => stop = true,
            n => fill = n.parse().unwrap_or(0),
        }
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        if fill > 0
            && let Err(e) = fill_first_page(&app.root().join(&rel), fill)
        {
            println!("bench pen fill_failed {e:#}");
            return bench_quit(&app);
        }
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench pen no_tab");
            return bench_quit(&app);
        };
        if stop && let Some(vault) = app.vault() {
            let _ = vault.stop_pdf_text();
        }
        // The pass starts behind the walk.
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        app.pdf_mode(pdfview::Mode::Pen);
        glib::timeout_future(Duration::from_secs(2)).await;
        // The file's strokes and their points.
        let inks = |pdf: &pdftab::PdfTab| {
            let doc = accent_core::pdf::PdfDoc::open(pdf.path()).ok();
            let inks = doc.map_or(Vec::new(), |d| {
                (0..d.page_count())
                    .flat_map(|p| d.inks(p).unwrap_or_default())
                    .collect()
            });
            let points = inks.iter().map(|i| i.points.len()).sum::<usize>();
            (inks.len(), points)
        };
        let before = inks(&pdf);
        let view = pdf.views()[0].clone();
        let reader = view.downcast_ref::<pdfview::PdfView>().unwrap().clone();
        let taken = || reader.drawn_points();
        let taken_before = taken();
        let (heard_before, cpu_before) = (reader.inks_heard(0).0, render_cpu_ms());
        let saves_before = pdf.saves();
        listen(true);
        let frames = Rc::new(RefCell::new(Vec::new()));
        let tick = view.add_tick_callback({
            let frames = frames.clone();
            move |_, _| {
                frames.borrow_mut().push(Instant::now());
                glib::ControlFlow::Continue
            }
        });
        // A motion with the button up ends a stroke's run of them.
        let motions = Rc::new(RefCell::new(Vec::new()));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let motions = motions.clone();
            move |m, _, _| {
                let down = m
                    .current_event_state()
                    .contains(gdk::ModifierType::BUTTON1_MASK);
                motions.borrow_mut().push(down.then(Instant::now));
            }
        });
        view.add_controller(motion.clone());
        let (sx, sy) = app.window.surface_transform();
        let on_screen = |x: f64, y: f64| {
            let p = graphene::Point::new(x as f32, y as f32);
            let p = view.compute_point(&app.window, &p).unwrap_or(p);
            (f64::from(p.x()) + sx, f64::from(p.y()) + sy)
        };
        let (w, h) = (f64::from(view.width()), f64::from(view.height()));
        // Each stroke a wave across the middle of the view, 60 motions long.
        let mut steps = Vec::new();
        for k in 0..24 {
            let y = h * (0.2 + 0.6 * f64::from(k % 12) / 12.0);
            for i in 0..=60 {
                let t = f64::from(i) / 60.0;
                let (x, y) = on_screen(w * (0.2 + 0.6 * t), y + 12.0 * (t * 25.0).sin());
                steps.push(format!("move {x:.0} {y:.0}"));
                if i == 0 {
                    steps.push("down".to_string());
                }
                steps.push("sleep 0.01".to_string());
            }
            steps.push("up; sleep 0.2".to_string());
        }
        let running = app.statusbar.pdf_text_control().is_visible();
        super::git::xtest(&steps.join("; ")).await;
        tick.remove();
        view.remove_controller(&motion);
        pdf.flush();
        written(&app).await;
        let heard = reader.inks_heard(0).0 - heard_before;
        let cpu = render_cpu_ms() - cpu_before;
        // The watcher's news of the last save, and a text pass it set off, land after it.
        glib::timeout_future(Duration::from_secs(2)).await;
        println!(
            "bench pen heard saves={} {}",
            pdf.saves() - saves_before,
            listen(false)
        );
        let gaps = |times: &[Option<Instant>]| {
            let mut gaps: Vec<f64> = times
                .windows(2)
                .filter_map(|w| Some(w[1]?.duration_since(w[0]?).as_secs_f64() * 1e3))
                .collect();
            gaps.sort_by(f64::total_cmp);
            let at = |q: f64| gaps.get((gaps.len() as f64 * q) as usize).copied();
            format!(
                "n={} p50_ms={:.0} p90_ms={:.0} p99_ms={:.0} max_ms={:.0} over50={}",
                gaps.len(),
                at(0.5).unwrap_or(0.0),
                at(0.9).unwrap_or(0.0),
                at(0.99).unwrap_or(0.0),
                gaps.last().copied().unwrap_or(0.0),
                gaps.iter().filter(|&&g| g > 50.0).count()
            )
        };
        let frames: Vec<Option<Instant>> = frames.borrow().iter().copied().map(Some).collect();
        let after = inks(&pdf);
        let inked = after.0 - before.0;
        println!(
            "bench pen text_pass={running} rss_mb={} frames {} motions {} strokes=24 inked={inked} \
             taken_per_stroke={} stored_per_stroke={}",
            super::notebook::rss_mb(),
            gaps(&frames),
            gaps(&motions.borrow()),
            (taken() - taken_before) / 24,
            (after.1 - before.1) / inked.max(1)
        );
        println!(
            "bench pen render strokes_before={} heard_per_stroke={} cpu_ms_per_stroke={:.1}",
            before.0,
            heard / 24,
            cpu / 24.0
        );
        // The first page's strokes as the view holds them and as the file has them, each by its
        // length and box, and the names the view holds.
        let shapes = |inks: Vec<accent_core::pdf::InkShape>| {
            let mut out: Vec<String> = (inks.iter())
                .map(|i| {
                    let b = i.bounds;
                    let edges = [b.left, b.top, b.right, b.bottom].map(|e| (e * 10.0).round());
                    format!("{}:{edges:?}", i.points.len())
                })
                .collect();
            out.sort();
            out
        };
        let held = || {
            let (_, held) = reader.inks_heard(0);
            let (names, inks): (std::collections::BTreeSet<u32>, Vec<_>) =
                held.unwrap_or_default().into_iter().unzip();
            (names, shapes(inks))
        };
        let filed = || {
            let doc = accent_core::pdf::PdfDoc::open(pdf.path()).ok();
            shapes(doc.and_then(|d| d.inks(0).ok()).unwrap_or_default())
        };
        let agree = |step: &str| {
            let file = filed();
            println!(
                "bench pen {step} strokes={} agrees={}",
                file.len(),
                held().1 == file
            );
        };
        agree("drawn");
        // A fresh list, which the eraser's mode asks for, names the strokes as the view did.
        let names = held().0;
        app.pdf_mode(pdfview::Mode::Eraser);
        glib::timeout_future(Duration::from_secs(2)).await;
        println!("bench pen named kept={}", held().0 == names);
        // The page lists the render thread read and sent for what follows, and its CPU time,
        // once it has gone quiet: the tiles and the lists of a step land after its save.
        let spent = Cell::new((reader.lists_heard(), render_cpu_ms()));
        let spent = async |step: &str| {
            let mut last = (reader.lists_heard(), render_cpu_ms());
            for _ in 0..40 {
                glib::timeout_future(Duration::from_millis(500)).await;
                let now = (reader.lists_heard(), render_cpu_ms());
                if now.0 == last.0 && now.1 - last.1 < 5.0 {
                    break;
                }
                last = now;
            }
            let (lists, cpu) = spent.replace((reader.lists_heard(), render_cpu_ms()));
            println!(
                "bench pen {step} lists={} render_cpu_ms={:.0}",
                reader.lists_heard() - lists,
                render_cpu_ms() - cpu
            );
        };
        let mut steps = Vec::new();
        for i in 0..=60 {
            let (x, y) = on_screen(w * 0.5, h * (0.1 + 0.8 * f64::from(i) / 60.0));
            steps.push(format!("move {x:.0} {y:.0}"));
            if i == 0 {
                steps.push("down".to_string());
            }
            steps.push("sleep 0.01".to_string());
        }
        steps.push("up".to_string());
        super::git::xtest(&steps.join("; ")).await;
        pdf.flush();
        written(&app).await;
        agree("erased");
        spent("erased").await;
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-undo", None);
        pdf.flush();
        written(&app).await;
        agree("undone");
        spent("undone").await;
        // The first stroke, taken by where it starts and moved down.
        let file = filed();
        app.pdf_mode(pdfview::Mode::Select);
        glib::timeout_future(Duration::from_secs(2)).await;
        let (x, y) = on_screen(w * 0.2, h * 0.2);
        super::git::xtest(&format!(
            "move {x:.0} {y:.0}; down; sleep 0.1; move {x:.0} {:.0}; sleep 0.1; \
             move {x:.0} {:.0}; sleep 0.1; up",
            y + 20.0,
            y + 40.0
        ))
        .await;
        pdf.flush();
        written(&app).await;
        agree("moved");
        println!("bench pen moved changed={}", filed() != file);
        // The stroke Select holds turned a quarter clockwise by its ring, Shift held, then undone
        // and made again: the one stroke taller than it is wide is that one.
        let tall = || {
            let doc = accent_core::pdf::PdfDoc::open(pdf.path()).ok();
            let inks = doc.and_then(|d| d.inks(0).ok()).unwrap_or_default();
            (inks.iter())
                .filter(|i| i.bounds.height() > i.bounds.width())
                .count()
        };
        let held = reader.picked_box().map(|(page, b, _)| (page, b));
        if let Some(r) = held.and_then(|(page, b)| reader.widget_rect(page, &b)) {
            let (l, t) = (f64::from(r.x()), f64::from(r.y()));
            let (w, h) = (f64::from(r.width()), f64::from(r.height()));
            let (cx, cy) = (l + w / 2.0, t + h / 2.0);
            let (ox, oy) = (l + w + 12.0 - cx, t - 12.0 - cy);
            let mut steps = vec!["keydown shift".to_string()];
            for k in 0..=3 {
                let (sin, cos) = (std::f64::consts::FRAC_PI_2 * f64::from(k) / 3.0).sin_cos();
                let (x, y) = on_screen(cx + ox * cos - oy * sin, cy + ox * sin + oy * cos);
                steps.push(format!("move {x:.0} {y:.0}"));
                if k == 0 {
                    steps.push("down".to_string());
                }
                steps.push("sleep 0.1".to_string());
            }
            steps.push("up; keyup shift".to_string());
            super::git::xtest(&steps.join("; ")).await;
        }
        pdf.flush();
        written(&app).await;
        agree("turned");
        let turned = tall();
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-undo", None);
        pdf.flush();
        written(&app).await;
        let undone = tall();
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-redo", None);
        pdf.flush();
        written(&app).await;
        println!(
            "bench pen turned held={} tall={turned} undone={undone} redone={}",
            held.is_some(),
            tall()
        );
        // The file broken under the tab and mended: the page erased on is listed again for the
        // tool in hand, by a render thread that opened it afresh.
        let whole = std::fs::read(pdf.path()).unwrap_or_default();
        let _ = std::fs::write(pdf.path(), b"");
        let waited = until("pen", "broken", 10_000, || pdf.waiting().0.map(drop))
            .await
            .is_some();
        println!("bench pen broken waited={waited}");
        let _ = std::fs::write(pdf.path(), &whole);
        until("pen", "reopened", 10_000, || {
            pdf.waiting().0.is_none().then_some(())
        })
        .await;
        glib::timeout_future(Duration::from_secs(2)).await;
        agree("reopened");
        bench_quit(&app);
    });
}

thread_local! {
    /// What the window heard from the vault while a drill listened: see [`heard`].
    static HEARD: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// A vault event on its way to `App::on_event`, written down while a drill listens: a changed
/// file by its path, a pass of the PDF text reader by its start, anything else by its kind.
pub(crate) fn heard(event: &accent_api::Event) {
    HEARD.with_borrow_mut(|heard| {
        let Some(heard) = heard else {
            return;
        };
        heard.push(match event {
            accent_api::Event::FileChanged(rel) => format!("FileChanged({rel})"),
            accent_api::Event::PdfText {
                done: 0,
                running: true,
                ..
            } => "PdfText(start)".to_string(),
            other => {
                let debug = format!("{other:?}");
                debug
                    .split(['(', ' ', '{'])
                    .next()
                    .unwrap_or_default()
                    .to_string()
            }
        });
    });
}

/// Start writing down what the window hears ([`heard`]), or stop and say how many of each.
fn listen(on: bool) -> String {
    let heard = HEARD.replace(on.then(Vec::new)).unwrap_or_default();
    let mut counts = std::collections::BTreeMap::new();
    for event in heard {
        *counts.entry(event).or_insert(0) += 1;
    }
    let counts: Vec<String> = counts.iter().map(|(e, n)| format!("{e}={n}")).collect();
    counts.join(" ")
}

/// `n` short strokes down the left edge of `path`'s first page, written into the file.
pub(super) fn fill_first_page(path: &std::path::Path, n: usize) -> anyhow::Result<()> {
    use accent_core::pdf;
    let mut doc = pdf::PdfDoc::open(path)?;
    let (_, height) = doc.page_size(0)?;
    let style = pdf::InkStyle {
        width: 1.0,
        rgba: [0, 0, 0, 255],
        multiply: false,
    };
    for i in 0..n {
        let y = 10.0 + (height - 20.0) * i as f32 / n as f32;
        let line = pdf::Shape::Line {
            a: (6.0, y),
            b: (24.0, y),
        };
        doc.add_shape(0, line, style)?;
    }
    doc.save_to(path, None)?;
    Ok(())
}

/// Create New Drawing through its dialog response, choose the last size and report disk content,
/// opened tab and active tool. Emit `response` directly because Xvfb cannot activate
/// `AdwAlertDialog` buttons; this covers the handler and resulting document, not pointer
/// interaction.
pub(super) fn bench_drawing(app: &Rc<App>, file: bool) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        online(&app).await;
        let action = match file {
            true => "win.new-file",
            false => "win.new-drawing",
        };
        let _ = WidgetExt::activate_action(&app.window, action, None);
        for _ in 0..40 {
            if app.window.visible_dialog().is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        let Some(dialog) = app
            .window
            .visible_dialog()
            .and_then(|d| d.downcast::<adw::AlertDialog>().ok())
        else {
            println!("bench drawing no_dialog");
            return bench_quit(&app);
        };
        let form = dialog.extra_child();
        let entry = form
            .as_ref()
            .and_then(|form| find_widget(form, &|w| w.is::<gtk::Entry>()))
            .and_downcast::<gtk::Entry>();
        // New File shows the sizes only once the name says PDF.
        let shown = || {
            let shown = |w: &gtk::Widget| {
                w.is::<gtk::DropDown>() && w.parent().is_some_and(|row| row.is_visible())
            };
            form.as_ref()
                .and_then(|form| find_widget(form, &shown))
                .and_downcast::<gtk::DropDown>()
        };
        if file && let Some(entry) = &entry {
            println!("bench drawing sizes_before_name={}", shown().is_some());
            entry.set_text("Blank.pdf");
        }
        let typed = entry.map(|entry| entry.text());
        let sizes = shown();
        let listed = sizes.as_ref().and_then(|d| d.model()).map(|m| m.n_items());
        println!(
            "bench drawing heading={:?} typed={typed:?} sizes={listed:?}",
            dialog.heading()
        );
        // The last shape, which is the one with arithmetic behind it: the window's proportions.
        let Some(sizes) = sizes else {
            println!("bench drawing no_sizes");
            return bench_quit(&app);
        };
        sizes.set_selected(3);
        dialog.emit_by_name::<()>("response", &[&"confirm"]);
        glib::timeout_future(Duration::from_millis(800)).await;
        let Some(pdf) = app.active_pdf() else {
            println!("bench drawing no_tab");
            return bench_quit(&app);
        };
        let sizes = accent_core::pdf::PdfDoc::open(pdf.path())
            .and_then(|doc| doc.page_sizes())
            .unwrap_or_default();
        println!(
            "bench drawing made key={:?} tool={:?} window={}x{} on_disk={sizes:?} in_vault {}",
            pdf.key(),
            pdf.mode_label(),
            app.window.width(),
            app.window.height(),
            vault_pages(&app, &pdf.key()),
        );
        bench_quit(&app);
    });
}

/// Insert Sketch from the note `rel`: what the note gained, the tab it opened beside it with the
/// tool in hand, and what the vault holds under that name — on a remote vault the host's copy.
pub(super) fn bench_sketch(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        online(&app).await;
        app.open_path(&rel);
        let Some(tab) = until("sketch", "note", 10_000, || app.tab_for(&rel)).await else {
            println!("bench sketch no_note");
            return bench_quit(&app);
        };
        let before = tab.text();
        let started = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.insert-sketch", None);
        let held = started.elapsed().as_millis();
        let sketch = || app.active_pdf().filter(|pdf| pdf.page_count() > 0);
        let pdf = until("sketch", "tab", 10_000, sketch).await;
        let Some(pdf) = pdf else {
            println!(
                "bench sketch no_tab held_ms={held} note_changed={} said={} {:?}",
                tab.text() != before,
                app.toasted.get(),
                bench_said(&app)
            );
            return bench_quit(&app);
        };
        println!(
            "bench sketch made held_ms={held} after_ms={} key={:?} embedded={} tool={:?} \
             in_vault {} said={}",
            started.elapsed().as_millis(),
            pdf.key(),
            tab.text().contains(&format!("![[{}]]", pdf.key())),
            pdf.mode_label(),
            vault_pages(&app, &pdf.key()),
            app.toasted.get()
        );
        bench_quit(&app);
    });
}
