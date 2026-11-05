//! Where a drill finds a note's preview: its preview tab, beside the note.

use super::*;

/// The preview tab a drill reads: the window's first.
pub(super) fn shown(app: &App) -> Option<Rc<crate::rendered::Rendered>> {
    app.rendereds().into_iter().next()
}

/// The active note's preview tab in a pane on its right and the note in front on the left, as
/// both are read side by side; the note waited for while it is still opening.
pub(super) async fn beside(app: &Rc<App>) {
    let t = Instant::now();
    while app.active().is_none() && t.elapsed() < Duration::from_secs(10) {
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    let Some(tab) = app.active() else {
        return;
    };
    let rendered = app.rendered_of(&tab.page).unwrap_or_else(|| {
        app.open_rendered(&crate::rendered::Note::Tab(tab.clone()), Opened::Kept)
    });
    if let Some(pane) = app.pane_of(&tab.page)
        && pane.has(&rendered.page)
    {
        app.split_page(&pane, Side::Right, &rendered.page);
    }
    // The keyboard too, which the split handed the preview on an idle.
    app.reveal_page(&tab.page);
    if let Some(pane) = app.pane_of(&tab.page) {
        app.focus_document(&pane);
    }
}
