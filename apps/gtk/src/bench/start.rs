//! A drill over the start screen, which a launch without a vault shows and which has no `App`.

use super::*;

/// `ACCENT_BENCH_START=1`: Remove from Recents pressed from the keyboard, the button focused and
/// clicked as Tab and Space would, and where the keyboard went each time. Point it at a config
/// whose recent vaults are four folders that exist; it forgets all four. The second row goes to
/// the third (`focus=` its name), the last to the one before it, the one the search shows while
/// the other is hidden to the search (`focus=search`), and the last left to Open Folder….
pub(crate) fn bench_start(window: &adw::ApplicationWindow) {
    let window = window.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(500)).await;
        let root = window.clone().upcast::<gtk::Widget>();
        let rows = || {
            let list = find_widget(&root, &|w| w.is::<gtk::ListBox>());
            std::iter::successors(list.and_then(|l| l.first_child()), |w| w.next_sibling())
                .filter_map(|w| w.downcast::<adw::ActionRow>().ok())
                .collect::<Vec<_>>()
        };
        let search =
            find_widget(&root, &|w| w.is::<gtk::SearchEntry>()).and_downcast::<gtk::SearchEntry>();
        let (shown, Some(search)) = (rows(), search) else {
            println!("bench start_forget none");
            return window.close();
        };
        if shown.len() != 4 {
            println!("bench start_forget rows={}", shown.len());
            return window.close();
        }
        forget(&window, &shown[1]).await;
        forget(&window, &shown[3]).await;
        // Only the third row matches: the first is hidden when the third goes.
        search.set_text(&shown[2].title());
        glib::timeout_future(Duration::from_millis(300)).await;
        forget(&window, &shown[2]).await;
        search.set_text("");
        glib::timeout_future(Duration::from_millis(300)).await;
        forget(&window, &shown[0]).await;
        window.close();
    });
}

/// Remove `row` from the recents as the keyboard does, and say where the keyboard is a frame
/// later, past where GTK would have moved a focus left on a removed widget.
async fn forget(window: &adw::ApplicationWindow, row: &adw::ActionRow) {
    let button = find_widget(row.upcast_ref(), &|w| {
        w.tooltip_text().as_deref() == Some("Remove from Recents")
    })
    .and_downcast::<gtk::Button>();
    if let Some(button) = button {
        button.grab_focus();
        button.emit_clicked();
    }
    glib::timeout_future(Duration::from_millis(200)).await;
    let focus = gtk::prelude::GtkWindowExt::focus(window);
    let named = focus.as_ref().map_or("none".to_string(), |f| {
        match (
            f.downcast_ref::<adw::ActionRow>(),
            f.ancestor(gtk::SearchEntry::static_type()),
        ) {
            (Some(row), _) => row.title().to_string(),
            (None, Some(_)) => "search".to_string(),
            _ => f
                .downcast_ref::<gtk::Button>()
                .and_then(|b| b.label())
                .map_or(f.type_().name().to_string(), |l| l.to_string()),
        }
    });
    println!("bench start_forget {} focus={named}", row.title());
}
