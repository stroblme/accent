//! A drill over an animated GIF in the preview and in an image tab: it plays while drawn, and
//! its timer stops once nothing draws it.

use super::*;
use crate::look::Animated;

/// `ACCENT_BENCH_ANIMATION=1` writes [`crate::look::TWO_FRAMES`] and a note embedding it into the
/// vault, then prints, for the preview beside the note and for the GIF's own tab, how many frames
/// it moved on in 600 ms shown, and in 600 ms hidden (the preview's widget, then the tab behind
/// the note) with whether its timer still runs; for the tab, whether its animation outlives the
/// tab's close.
pub(super) fn bench_animation(app: &Rc<App>) {
    let written = std::fs::write(app.root().join("anim.gif"), crate::look::TWO_FRAMES)
        .and_then(|_| std::fs::write(app.root().join("anim.md"), "![[anim.gif]]\n"));
    if let Err(e) = written {
        println!("bench animation wrote=false {e}");
        return bench_quit(app);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        for _ in 0..600 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        app.open_path("anim.md");
        super::preview::beside(&app).await;
        preview(&app).await;
        tab(&app).await;
        bench_quit(&app);
    });
}

async fn preview(app: &Rc<App>) {
    let playing = || {
        let rendered = super::preview::shown(app)?;
        let preview = rendered.preview();
        let (figures, _) = preview.as_ref()?.figures();
        figures.first()?.playing
    };
    if until("animation", "preview", 2000, || playing().is_some())
        .await
        .is_none()
    {
        return println!("bench animation preview none");
    }
    let shown = moves(playing).await;
    let widget =
        super::preview::shown(app).and_then(|r| r.preview().as_ref().map(|p| p.widget().clone()));
    let Some(widget) = widget else {
        return println!("bench animation preview none");
    };
    widget.set_visible(false);
    let hidden = moves(playing).await;
    let running = playing().is_some_and(|(_, on)| on);
    widget.set_visible(true);
    let again = moves(playing).await;
    println!("bench animation preview shown={shown} hidden={hidden} timer={running} again={again}");
}

async fn tab(app: &Rc<App>) {
    let note = app.active();
    app.open_path("anim.gif");
    let animated = || {
        let image = app.images().into_iter().find(|i| i.key() == "anim.gif")?;
        crate::zoom::picture_of(&image.page)?
            .paintable()
            .and_downcast::<Animated>()
    };
    if until("animation", "tab", 2000, || animated().is_some())
        .await
        .is_none()
    {
        return println!("bench animation tab none");
    }
    let playing = || animated().map(|a| a.playing());
    let shown = moves(playing).await;
    if let Some(note) = &note {
        app.reveal_page(&note.page);
    }
    let hidden = moves(playing).await;
    let running = playing().is_some_and(|(_, on)| on);
    let weak = animated().map(|a| a.downgrade());
    if let Some(image) = app.images().into_iter().find(|i| i.key() == "anim.gif") {
        app.close_page(&image.page);
    }
    glib::timeout_future(Duration::from_millis(300)).await;
    let outlived = weak.is_some_and(|w| w.upgrade().is_some());
    println!(
        "bench animation tab shown={shown} hidden={hidden} timer={running} outlived={outlived}"
    );
}

/// How many times the frame on show changed in 600 ms, the first 250 ms let pass so a timer set
/// before is spent.
async fn moves(playing: impl Fn() -> Option<(usize, bool)>) -> u32 {
    glib::timeout_future(Duration::from_millis(250)).await;
    let (mut last, mut moved) = (playing().map(|p| p.0), 0);
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(600) {
        glib::timeout_future(Duration::from_millis(10)).await;
        let now = playing().map(|p| p.0);
        moved += u32::from(now != last);
        last = now;
    }
    moved
}
