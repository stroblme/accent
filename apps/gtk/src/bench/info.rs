//! The Info pane drill: which sections it shows over each kind of tab, and what they say.

use super::*;

/// Long enough for a file to open and the References answer, 300 ms behind it, to land.
const SETTLE: Duration = Duration::from_millis(1500);

/// `ACCENT_BENCH_INFO=<rel>[,<rel>…]` brings the Info pane to the front, prints it before any tab
/// is open, opens the Details section, then opens each file in turn and prints the pane again
/// once the file's answers have landed: the page it shows, each section as
/// `Title:open|shut|hidden` with its count, the dividers as `position/height`, and the Details
/// rows as `Group.Name=value`. Last, over the last file, it drags the upper divider to 200 px and
/// folds the sections as clicks on their headers would, printing each fold: the divider comes
/// back to 200 px with all three open again.
pub(super) fn bench_info(app: &Rc<App>, rels: &str) {
    app.show_pane("info");
    let (app, rels) = (app.clone(), rels.to_string());
    glib::spawn_future_local(async move {
        let sidebar = app.sidebar.get().expect("a sidebar");
        glib::timeout_future(SETTLE).await;
        print_info(&app, "none");
        sidebar.fold_section("details", true);
        for rel in rels.split(',') {
            app.open_path(rel);
            glib::timeout_future(SETTLE).await;
            print_info(&app, rel);
        }
        // The pane's own divider is the first paned in it; the others are inside that.
        let divider = find_widget(sidebar.widget(), &|w| w.is::<adw::ViewStack>())
            .and_downcast::<adw::ViewStack>()
            .and_then(|stack| stack.child_by_name("info"))
            .and_then(|info| find_widget(&info, &|w| w.is::<gtk::Paned>()))
            .and_downcast::<gtk::Paned>()
            .expect("the divider");
        // A drag, as `paned::watch` marks one while the handle is held.
        divider.add_css_class(crate::paned::DRAGGING);
        divider.set_position(200);
        divider.remove_css_class(crate::paned::DRAGGING);
        for open in [
            [false, true, true],
            [true, false, true],
            [true, true, false],
            [false, false, false],
            [true, true, true],
        ] {
            for (name, open) in ["references", "tags", "details"].into_iter().zip(open) {
                sidebar.fold_section(name, open);
            }
            glib::timeout_future(Duration::from_millis(300)).await;
            print_info(&app, "fold");
        }
        bench_quit(&app);
    });
}

fn print_info(app: &Rc<App>, rel: &str) {
    if let Some(sidebar) = app.sidebar.get() {
        let (state, details) = sidebar.info_state();
        println!("bench info {rel} {state}");
        if !details.is_empty() {
            println!("bench info {rel} details {}", details.join(" | "));
        }
    }
}
