//! The toasts' drill: how many stand at once, in what order, and how long.

use super::*;

/// `ACCENT_BENCH_CHROME=toasts` raises four toasts at once and prints the ones standing, top to
/// bottom: `four` must read `["four", "three", "two"]`, the newest on top and the first sent away.
/// Then two under one key, the second taking the first's place: `keyed` must read
/// `["keyed two", "four", "three"]`. Last, 5 s after the first four and before the keyed one's
/// own 5 s are up, `timed` must read `["keyed two"]`: each goes on its own timeout.
pub(super) fn bench_toasts(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let start = Instant::now();
        for title in ["one", "two", "three", "four"] {
            app.toast(title);
        }
        // Past the slide in and out.
        glib::timeout_future(Duration::from_millis(500)).await;
        println!("bench toasts four shown={:?}", app.toasts.shown());
        app.add_toast(Toast::new("keyed one").key("bench"));
        app.add_toast(Toast::new("keyed two").key("bench"));
        glib::timeout_future(Duration::from_millis(500)).await;
        println!("bench toasts keyed shown={:?}", app.toasts.shown());
        glib::timeout_future(Duration::from_millis(5250).saturating_sub(start.elapsed())).await;
        println!("bench toasts timed shown={:?}", app.toasts.shown());
        bench_quit(&app);
    });
}
