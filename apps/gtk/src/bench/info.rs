//! The Info pane drill: which sections it shows over each kind of tab, and what they say.

use super::*;

/// Long enough for a file to open and the References answer, 300 ms behind it, to land.
const SETTLE: Duration = Duration::from_millis(1500);

/// `ACCENT_BENCH_INFO=<rel>[,<rel>…]` brings the Info pane to the front, prints it before any tab
/// is open, then opens each file in turn and prints it again once the file's answers have landed:
/// the page the pane shows, each section as `Title:open|shut|hidden` with its count, and the
/// divider as `position/height`. Last, over the last file, it drags the divider to 200 px and
/// folds the sections as clicks on their headers would — References shut, Tags shut, both shut,
/// both open — printing each: the divider comes back to 200 px.
pub(super) fn bench_info(app: &Rc<App>, rels: &str) {
    app.show_pane("info");
    let (app, rels) = (app.clone(), rels.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(SETTLE).await;
        print_info(&app, "none");
        for rel in rels.split(',') {
            app.open_path(rel);
            glib::timeout_future(SETTLE).await;
            print_info(&app, rel);
        }
        let sidebar = app.sidebar.get().expect("a sidebar");
        // The pane's own divider is the first paned in it; the Tags section's is inside that.
        let divider = find_widget(sidebar.widget(), &|w| w.is::<adw::ViewStack>())
            .and_downcast::<adw::ViewStack>()
            .and_then(|stack| stack.child_by_name("info"))
            .and_then(|info| find_widget(&info, &|w| w.is::<gtk::Paned>()))
            .and_downcast::<gtk::Paned>()
            .expect("the divider");
        divider.set_position(200);
        for (references, tags) in [(false, true), (true, false), (false, false), (true, true)] {
            sidebar.fold_section("references", references);
            sidebar.fold_section("tags", tags);
            glib::timeout_future(Duration::from_millis(300)).await;
            print_info(&app, "fold");
        }
        bench_quit(&app);
    });
}

fn print_info(app: &Rc<App>, rel: &str) {
    if let Some(sidebar) = app.sidebar.get() {
        println!("bench info {rel} {}", sidebar.info_state());
    }
}
