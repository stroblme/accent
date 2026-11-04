//! `ACCENT_BENCH_EDITOR`: files opened in a terminal editor. Only `build-aux/fake-editor.sh`
//! ever runs as one, linked into a scratch `HOME`'s `bin` as `nano` (and `kak`, which ends as a
//! missing program does), which that `HOME`'s `.profile` puts first on the login shell's `PATH`.

use super::*;
use accent_core::config::Editor;

/// `=prefs`: Preferences → Text Editor lists the fake `nano` once the shell has been asked, and
/// picking it is the config's editor.
pub(super) fn bench_editor(app: &Rc<App>, step: &str) {
    let (app, step) = (app.clone(), step.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        if step.as_str() == "prefs" {
            prefs(&app).await;
        }
        bench_quit(&app);
    });
}

/// Wait up to `ms` for `done`.
async fn until(done: impl Fn() -> bool, ms: u64) -> bool {
    for _ in 0..ms / 50 {
        if done() {
            return true;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    done()
}

/// Every widget under `root` of type `T`, in order.
fn all<T: IsA<gtk::Widget>>(root: &gtk::Widget) -> Vec<T> {
    let mut found: Vec<T> = root.clone().downcast::<T>().into_iter().collect();
    let mut child = root.first_child();
    while let Some(widget) = child {
        found.extend(all::<T>(&widget));
        child = widget.next_sibling();
    }
    found
}

/// The names a combo row lists.
fn names(row: &adw::ComboRow) -> Vec<String> {
    let Some(model) = row.model().and_downcast::<gtk::StringList>() else {
        return Vec::new();
    };
    (0..model.n_items())
        .filter_map(|i| model.string(i))
        .map(|s| s.to_string())
        .collect()
}

async fn prefs(app: &Rc<App>) {
    let _ = WidgetExt::activate_action(&app.window, "win.preferences", None);
    glib::timeout_future(Duration::from_millis(500)).await;
    let Some(dialog) = app.window.visible_dialog() else {
        return println!("bench editor prefs dialog=none");
    };
    let row = all::<adw::ComboRow>(dialog.upcast_ref())
        .into_iter()
        .find(|row| row.title() == "Text Editor");
    let Some(row) = row else {
        return println!("bench editor prefs row=none");
    };
    let listed = |name: &str| names(&row).iter().filter(|n| *n == name).count();
    until(|| listed("Nano") > 0, 10_000).await;
    if let Some(at) = names(&row).iter().position(|n| n == "Nano") {
        row.set_selected(at as u32);
    }
    let chosen = app.config.borrow().editor;
    println!(
        "bench editor prefs nano={} chosen={}",
        listed("Nano"),
        match chosen {
            Editor::Nano => "nano",
            _ => crate::editors::name(chosen),
        }
    );
}
