//! A drill over images pasted and dropped into a note.

use super::*;
use accent_core::attachment;
use std::os::unix::fs::PermissionsExt;

/// `ACCENT_BENCH_ATTACH=<rel_note>,<rel_vault_png>,<rel_code>` puts an image on the real
/// clipboard and pastes it at the end of the note, undoes and redoes that, and pastes it again
/// over a selected word; then drops a PNG from outside the vault onto the text twice and
/// `<rel_vault_png>` once, each at the end with the caret at the start. After every step it
/// prints the note's last line and the files the attachment folder gained, with their modes.
/// Then it pastes into `<rel_code>`, which must take nothing, and ends in the split view for five
/// seconds, which is the time to take a screenshot of the preview. It writes into the vault, so
/// point it at a scratch one.
pub(super) fn bench_attach(app: &Rc<App>, arg: &str) {
    let [note, in_vault, code] = arg.splitn(3, ',').collect::<Vec<_>>()[..] else {
        println!("bench attach needs <rel_note>,<rel_vault_png>,<rel_code>");
        return bench_quit(app);
    };
    let (app, note, in_vault, code) = (
        app.clone(),
        note.to_string(),
        in_vault.to_string(),
        code.to_string(),
    );
    glib::spawn_future_local(async move {
        let Some(tab) = opened(&app, &note).await else {
            println!("bench attach no_tab {note}");
            return bench_quit(&app);
        };
        let root = app.root();
        let setting = app.vault().map(|v| v.config().attachment_folder);
        let dir = root.join(attachment::folder(&setting.unwrap_or_default(), &note));
        let before = listed(&dir);
        let step = |case: &str| {
            let text = tab
                .buffer
                .text(&tab.buffer.start_iter(), &tab.buffer.end_iter(), true);
            let last = text.lines().last().unwrap_or_default().to_string();
            let gained: Vec<String> = listed(&dir)
                .into_iter()
                .filter(|entry| !before.contains(entry))
                .map(|name| format!("{name} {:o}", mode(&dir.join(&name))))
                .collect();
            println!("bench attach {case} last={last:?} gained={gained:?}");
        };

        let texture = sample();
        tab.view.clipboard().set_texture(&texture);
        tab.buffer.place_cursor(&tab.buffer.end_iter());
        tab.view.emit_paste_clipboard();
        glib::timeout_future(Duration::from_millis(1500)).await;
        step("paste");
        tab.buffer.undo();
        step("undo");
        tab.buffer.redo();

        // Over a word: the image takes its place, where a URL would have made it a link.
        tab.buffer.insert(&mut tab.buffer.end_iter(), "\nover this");
        let end = tab.buffer.end_iter();
        tab.buffer
            .select_range(&tab.buffer.iter_at_offset(end.offset() - 4), &end);
        tab.view.emit_paste_clipboard();
        glib::timeout_future(Duration::from_millis(1500)).await;
        step("paste_over");

        // A file from outside the vault, made private so the copy's own mode shows.
        let outside = std::env::temp_dir().join("accent-bench-attach/shot.png");
        let _ = std::fs::create_dir_all(outside.parent().unwrap());
        std::fs::write(&outside, texture.save_to_png_bytes()).unwrap();
        let _ = std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600));
        for (case, file) in [
            ("drop_outside", outside.clone()),
            ("drop_outside_again", outside),
            ("drop_in_vault", root.join(&in_vault)),
        ] {
            tab.buffer.insert(&mut tab.buffer.end_iter(), "\n");
            tab.buffer.place_cursor(&tab.buffer.start_iter());
            drop_file(&tab, &file);
            glib::timeout_future(Duration::from_millis(1500)).await;
            step(case);
            let caret = tab.buffer.iter_at_mark(&tab.buffer.get_insert());
            println!(
                "bench attach {case} caret_at_end={}",
                caret.offset() == tab.buffer.end_iter().offset()
            );
        }

        // A code tab keeps GTK's paste, which takes no image.
        if let Some(code_tab) = opened(&app, &code).await {
            let chars = code_tab.buffer.char_count();
            let (beside, attached) = (
                listed(&root.join(attachment::folder("", &code))),
                listed(&dir),
            );
            code_tab.view.clipboard().set_texture(&texture);
            code_tab.view.emit_paste_clipboard();
            glib::timeout_future(Duration::from_millis(1500)).await;
            println!(
                "bench attach code_paste text_unchanged={} files_unchanged={}",
                code_tab.buffer.char_count() == chars,
                listed(&root.join(attachment::folder("", &code))) == beside
                    && listed(&dir) == attached,
            );
        }

        app.open_path(&note);
        app.set_mode(Mode::Split);
        println!("bench attach shot");
        glib::timeout_future(Duration::from_secs(5)).await;
        bench_quit(&app);
    });
}

/// The note's tab once it has opened, or `None` after five seconds.
async fn opened(app: &Rc<App>, rel: &str) -> Option<Rc<Tab>> {
    app.open_path(rel);
    for _ in 0..50 {
        if let Some(tab) = app.open_tabs().into_iter().find(|t| t.rel() == rel) {
            return Some(tab);
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    None
}

/// A drop of `file` from another application at the end of the note, through the view's own file
/// drop target: Xvfb carries a drag inside one process and not between two.
fn drop_file(tab: &Rc<Tab>, file: &Path) {
    let target = tab
        .view
        .observe_controllers()
        .into_iter()
        .flatten()
        .filter_map(|c| c.downcast::<gtk::DropTarget>().ok())
        .find(|t| {
            t.formats()
                .is_some_and(|f| f.contains_type(gdk::FileList::static_type()))
        })
        .expect("the note's file drop target");
    let end = tab.view.iter_location(&tab.buffer.end_iter());
    let (x, y) =
        tab.view
            .buffer_to_window_coords(gtk::TextWindowType::Widget, end.x() + 1, end.y() + 1);
    let list = gdk::FileList::from_array(&[gio::File::for_path(file)]);
    let value = glib::BoxedValue(list.to_value());
    target.emit_by_name::<bool>("drop", &[&value, &f64::from(x), &f64::from(y)]);
}

/// A small striped picture, so the preview's screenshot shows it is the file and not a hole.
fn sample() -> gdk::Texture {
    let (w, h) = (96usize, 48usize);
    let pixels: Vec<u8> = (0..w * h)
        .flat_map(|i| match (i % w) / 16 % 2 {
            0 => [53u8, 132, 228],
            _ => [246, 211, 45],
        })
        .collect();
    gdk::MemoryTexture::new(
        w as i32,
        h as i32,
        gdk::MemoryFormat::R8g8b8,
        &glib::Bytes::from_owned(pixels),
        w * 3,
    )
    .upcast()
}

fn listed(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).map_or(0, |m| m.permissions().mode() & 0o777)
}
