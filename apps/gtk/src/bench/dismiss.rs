//! A primary click outside a dialog (`dialogs::close_on_outside_press`), through real XTEST
//! clicks.

use super::*;

/// The dialogs a click outside closes, by the action that presents each.
const CLOSING: [(&str, &str); 4] = [
    ("files", "win.palette-files"),
    ("commands", "win.palette-commands"),
    ("recent", "win.open-recent"),
    ("preferences", "win.preferences"),
];

/// `ACCENT_BENCH_SWITCHER=dismiss:<relA>,<relB>` opens the two notes in one pane, `<relA>` in
/// front with its caret at the start, and presents each dialog in [`CLOSING`] in turn. For each it
/// prints `bench dismiss_ready <case> <x> <y>`, a point on screen outside the dialog, on `<relA>`'s
/// text or else on `<relB>`'s tab, for an XTEST click there (`build-aux/xtest.py :N "move x y;
/// down; up"`), then whether the dialog closed, whether the tab in front and its caret are what
/// they were, and whether the dialog was let go. Then a click on the palette's own entry, which
/// leaves it up; one outside Preferences with Restore Defaults' question over it, and one outside
/// an alert alone, both of which stay; and last the same two points with no dialog up, which move
/// the caret and switch the tab, so the points are shown to be live.
pub(super) fn bench_dismiss(app: &Rc<App>, rels: &str) {
    let Some((a, b)) = rels.split_once(',') else {
        return bench_quit(app);
    };
    let (app, a, b) = (app.clone(), a.to_string(), b.to_string());
    glib::spawn_future_local(async move {
        app.open_path(&b);
        app.open_path(&a);
        glib::timeout_future(Duration::from_millis(800)).await;
        let Some(tab) = app.active() else {
            return bench_quit(&app);
        };
        let window = app.window.clone().upcast::<gtk::Widget>();
        let (sx, sy) = app.window.surface_transform();
        // The text view's right margin, low in the window above the status bar, and the middle of
        // `<relB>`'s title in the tab bar, both in window coordinates.
        let on_text = tab
            .view
            .compute_point(
                &window,
                &graphene::Point::new(tab.view.width() as f32 - 24.0, 0.0),
            )
            .map(|p| graphene::Point::new(p.x(), window.height() as f32 - 80.0));
        let name = Path::new(&b)
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        let on_tab = find_widget(&window, &|w| {
            w.downcast_ref::<gtk::Label>()
                .is_some_and(|l| Some(l.text().to_string()) == name)
                && w.ancestor(adw::TabBar::static_type()).is_some()
        })
        .and_then(|label| {
            let middle =
                graphene::Point::new(label.width() as f32 / 2.0, label.height() as f32 / 2.0);
            label.compute_point(&window, &middle)
        });
        let under = |p: &graphene::Point| {
            window
                .pick(p.x() as f64, p.y() as f64, gtk::PickFlags::DEFAULT)
                .map_or("none".to_string(), |w| w.css_name().to_string())
        };
        let aims = [on_text, on_tab].into_iter().flatten().collect::<Vec<_>>();
        for p in &aims {
            println!(
                "bench dismiss aim {:.0},{:.0} under={}",
                p.x(),
                p.y(),
                under(p)
            );
        }
        let ready = |case: &str, p: &graphene::Point| {
            println!(
                "bench dismiss_ready {case} {:.0} {:.0}",
                p.x() as f64 + sx,
                p.y() as f64 + sy
            )
        };
        let front = || app.active().map(|t| t.rel());
        let caret = || tab.buffer.cursor_position();

        for (turn, (case, action)) in CLOSING.into_iter().enumerate() {
            tab.buffer.place_cursor(&tab.buffer.start_iter());
            let _ = WidgetExt::activate_action(&app.window, action, None);
            glib::timeout_future(Duration::from_millis(600)).await;
            let Some(dialog) = app.window.visible_dialog() else {
                println!("bench dismiss {case} no_dialog");
                continue;
            };
            let sheet = dialog.child().and_then(|c| c.compute_bounds(&window));
            // The text and the tab by turns, whichever the dialog leaves clear.
            let Some(aim) = aims
                .iter()
                .cycle()
                .skip(turn)
                .take(aims.len())
                .find(|p| !sheet.is_some_and(|r| r.contains_point(p)))
            else {
                println!("bench dismiss {case} no_aim");
                dialog.force_close();
                continue;
            };
            println!("bench dismiss {case} under={}", under(aim));
            let weak = dialog.downgrade();
            drop(dialog);
            ready(case, aim);
            wait(|| app.window.visible_dialog().is_some()).await;
            println!(
                "bench dismiss {case} closed={} front_same={} caret_same={} freed={}",
                app.window.visible_dialog().is_none(),
                front().as_deref() == Some(a.as_str()),
                caret() == 0,
                weak.upgrade().is_none(),
            );
            if let Some(dialog) = app.window.visible_dialog() {
                dialog.force_close();
            }
        }

        // A click on the palette's own entry is inside it.
        let _ = WidgetExt::activate_action(&app.window, "win.palette-files", None);
        glib::timeout_future(Duration::from_millis(600)).await;
        let entry = app
            .window
            .visible_dialog()
            .and_then(|d| find_widget(d.upcast_ref(), &|w| w.is::<gtk::SearchEntry>()));
        let middle = entry.as_ref().and_then(|e| {
            let p = graphene::Point::new(e.width() as f32 / 2.0, e.height() as f32 / 2.0);
            e.compute_point(&window, &p)
        });
        if let Some(p) = middle {
            ready("inside", &p);
            wait(|| app.window.visible_dialog().is_some()).await;
            println!(
                "bench dismiss inside closed={}",
                app.window.visible_dialog().is_none()
            );
        }
        if let Some(dialog) = app.window.visible_dialog() {
            dialog.force_close();
        }
        glib::timeout_future(Duration::from_millis(300)).await;

        // Restore Defaults' question over Preferences: the click is the question's, and both stay.
        let _ = WidgetExt::activate_action(&app.window, "win.preferences", None);
        glib::timeout_future(Duration::from_millis(600)).await;
        let prefs = app.window.visible_dialog();
        let restore = prefs.as_ref().and_then(|d| {
            find_widget(d.upcast_ref(), &|w| {
                w.downcast_ref::<adw::ButtonRow>()
                    .is_some_and(|r| r.title() == "Restore Defaults")
            })
        });
        if let (Some(prefs), Some(restore)) = (prefs, restore) {
            restore.emit_by_name::<()>("activated", &[]);
            glib::timeout_future(Duration::from_millis(600)).await;
            let alert = || {
                app.window
                    .visible_dialog()
                    .is_some_and(|d| d.is::<adw::AlertDialog>())
            };
            println!("bench dismiss stacked alert_up={}", alert());
            if let Some(aim) = aims.first() {
                ready("stacked", aim);
                wait(alert).await;
            }
            println!(
                "bench dismiss stacked alert_stayed={} preferences_stayed={}",
                alert(),
                prefs.root().is_some()
            );
            if let Some(dialog) = app.window.visible_dialog() {
                dialog.force_close();
            }
            glib::timeout_future(Duration::from_millis(300)).await;
            prefs.force_close();
            glib::timeout_future(Duration::from_millis(300)).await;
        }

        // An alert alone keeps its question.
        tab.buffer.place_cursor(&tab.buffer.start_iter());
        crate::dialogs::confirm(
            &app.window,
            "Discard Changes?",
            "A question a click outside never answers.",
            "Discard",
            true,
            || println!("bench dismiss alert answered"),
        );
        glib::timeout_future(Duration::from_millis(600)).await;
        if let Some(aim) = aims.first() {
            ready("alert", aim);
            wait(|| app.window.visible_dialog().is_some()).await;
        }
        println!(
            "bench dismiss alert stayed={} caret_same={}",
            app.window.visible_dialog().is_some(),
            caret() == 0
        );
        if let Some(dialog) = app.window.visible_dialog() {
            dialog.force_close();
        }
        glib::timeout_future(Duration::from_millis(300)).await;

        // With no dialog up the same points are live: the text takes the caret, the tab the pane.
        for (case, aim) in ["text", "tab"].into_iter().zip(&aims) {
            ready(case, aim);
            glib::timeout_future(Duration::from_millis(1500)).await;
            println!(
                "bench dismiss control {case} caret_moved={} front={}",
                caret() != 0,
                front().unwrap_or_default()
            );
        }
        bench_quit(&app);
    });
}

/// Up to four seconds for the click, for as long as `up` says the dialog it waits on is up, and
/// then the dialog's close.
async fn wait(up: impl Fn() -> bool) {
    for _ in 0..40 {
        glib::timeout_future(Duration::from_millis(100)).await;
        if !up() {
            break;
        }
    }
    glib::timeout_future(Duration::from_millis(300)).await;
}
