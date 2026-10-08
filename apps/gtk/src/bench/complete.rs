//! The completion popup against real providers, typed into a tab the way a key press types
//! (`ACCENT_BENCH_COMPLETE`): the note index's links and tags, and a language server's members.

use super::*;
use crate::completion::{self, Session};

/// `=note:<rel>` and `=code:<rel>[,<typed>]`, on a scratch vault under `/tmp` only: they type.
///
/// `note:` writes `complete-one.md` (tagged `#complete-tag`) and `complete-two.md`, then types into
/// the note at `rel`: `[[` and the rows it opens, `complete-t` one character at a time and what
/// it narrows to, Down and Tab and the line that leaves (`[[complete-two]]`, the `]]` the pair
/// left eaten once, however much was typed since the popup opened), Ctrl+Z, and the same for
/// `#c`.
///
/// `code:` types `typed` (`s.g` unless given) into the first blank line of the file at `rel`, one
/// character at a time, writing a C `main` there first where there is no such file, and prints
/// the rows after each character; then Down and the selected
/// row's documentation, Return and the file's first line with the caret's (what the row brought
/// with it: an import), Ctrl+Z and the same two lines, and last `x` and `.` 50 ms apart, the
/// second superseding the first's request at the server (`fake-lsp cancelled` under
/// `RUST_LOG=accent_lsp::stderr=debug`). Against `build-aux/fake-lsp.py`, configured for the
/// file's language, every line is fixed (`make drills`); against a real server it is what that
/// server answers, which with `typed` naming an unimported symbol shows its import landing.
///
/// Each step's latency from the keystroke to the rows is a `time complete …` line.
pub(super) fn bench_complete(app: &Rc<App>, arg: &str) {
    scratch_only(app, "ACCENT_BENCH_COMPLETE");
    let (app, arg) = (app.clone(), arg.to_string());
    glib::spawn_future_local(async move {
        for _ in 0..300 {
            if app.reconciled.get() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        match arg.split_once(':') {
            Some(("note", rel)) => bench_complete_note(&app, rel).await,
            Some(("code", rest)) => {
                let (rel, typed) = rest.split_once(',').unwrap_or((rest, "s.g"));
                bench_complete_code(&app, rel, typed).await;
            }
            _ => println!("bench complete unknown {arg:?}"),
        }
        bench_quit(&app);
    });
}

async fn bench_complete_note(app: &Rc<App>, rel: &str) {
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    for (note, text) in [
        ("complete-one.md", "# One\n\n#complete-tag\n"),
        ("complete-two.md", "# Two\n"),
    ] {
        if let Err(e) = vault.save(note, text, None) {
            println!("bench complete write_failed {note} {e:?}");
        }
    }
    let Some((tab, session)) = bench_complete_open(app, rel).await else {
        return;
    };
    // Until the index has both notes: what `[[` offers is the index's.
    for _ in 0..100 {
        tab.set_text("[[complete-]]");
        crate::lang::flush(tab.clone()).await;
        let at = accent_api::Pos {
            line: 0,
            character: 11,
        };
        if vault
            .completion(&tab.rel(), at, None)
            .await
            .is_ok_and(|a| a.items.len() >= 2)
        {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    let own = tab.text();

    tab.set_text("");
    bench_type(&tab, &session, "complete_link_open", "[[").await;
    bench_type(&tab, &session, "complete_link_narrow", "complete-t").await;
    bench_key(&tab, &session, "complete_link_down", gdk::Key::Down).await;
    bench_key(&tab, &session, "complete_link_tab", gdk::Key::Tab).await;
    tab.buffer.undo();
    println!("bench complete_link_undo {:?}", bench_line(&tab, 0));

    tab.set_text("");
    bench_type(&tab, &session, "complete_tag_open", "#").await;
    bench_type(&tab, &session, "complete_tag_narrow", "c").await;
    bench_key(&tab, &session, "complete_tag_down", gdk::Key::Down).await;
    bench_key(&tab, &session, "complete_tag_return", gdk::Key::Return).await;
    tab.set_text(&own);
}

async fn bench_complete_code(app: &Rc<App>, rel: &str, typed: &str) {
    let Some(vault) = app.vault().cloned() else {
        return;
    };
    let text = match vault.read(rel) {
        Ok((text, _)) => text,
        Err(_) => {
            let text = "int main(void)\n{\n    \n}\n".to_string();
            if let Err(e) = vault.save(rel, &text, None) {
                return println!("bench complete write_failed {rel} {e:?}");
            }
            text
        }
    };
    let Some(blank) = text.lines().position(|l| l.trim().is_empty()) else {
        return println!("bench complete no_blank_line {rel}");
    };
    let Some((tab, session)) = bench_complete_open(app, rel).await else {
        return;
    };
    let body = || {
        tab.buffer
            .place_cursor(&editor::line_end(&tab.buffer, blank as i32))
    };
    body();
    // Until the server answers at all: a real one indexes the project first.
    let at = crate::lang::pos_of(&editor::caret(&tab.buffer));
    for _ in 0..600 {
        if vault
            .completion(rel, at, None)
            .await
            .is_ok_and(|a| !a.items.is_empty())
        {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    for c in typed.chars() {
        bench_type(
            &tab,
            &session,
            &format!("complete_code_{c}"),
            &c.to_string(),
        )
        .await;
    }
    bench_key(&tab, &session, "complete_code_down", gdk::Key::Down).await;
    println!("bench complete_code_doc {:?}", session.doc());
    bench_key(&tab, &session, "complete_code_return", gdk::Key::Return).await;
    let line = editor::caret(&tab.buffer).line();
    println!(
        "bench complete_code_accepted {:?} {:?}",
        bench_line(&tab, 0),
        bench_line(&tab, line)
    );
    tab.buffer.undo();
    println!(
        "bench complete_code_undo {:?} {:?}",
        bench_line(&tab, 0),
        bench_line(&tab, blank as i32)
    );

    // A request typed past: the second ask drops the first while the server still has it.
    tab.set_text(&text);
    body();
    bench_char(&tab, 'x');
    glib::timeout_future(Duration::from_millis(50)).await;
    bench_type(&tab, &session, "complete_code_superseded", ".").await;
    tab.set_text(&text);
    if let Err(e) = app.write_tab(&tab, None) {
        println!("bench complete write_failed {e}");
    }
}

/// The tab at `rel` once it is open on the language layer, with the keyboard, and its session.
async fn bench_complete_open(app: &Rc<App>, rel: &str) -> Option<(Rc<Tab>, Rc<Session>)> {
    app.open_path(rel);
    for _ in 0..300 {
        let tab = app.open_tabs().into_iter().find(|tab| tab.rel() == rel);
        if let Some(tab) = tab.filter(|tab| tab.lang.support().is_some()) {
            tab.view.grab_focus();
            let session = completion::session(&tab)?;
            return Some((tab, session));
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    println!("bench complete no_tab {rel}");
    None
}

/// Type `text` one character per idle through the key chain, then print the rows once they
/// have settled.
async fn bench_type(tab: &Rc<Tab>, session: &Rc<Session>, step: &str, text: &str) {
    let t0 = Instant::now();
    for c in text.chars() {
        bench_char(tab, c);
        glib::timeout_future(Duration::from_millis(1)).await;
    }
    bench_settle(session).await;
    println!("time {step} ms={:.0}", ms_since(t0));
    println!(
        "bench {step} {:?} shown={}",
        bench_rows(session).0,
        session.is_shown()
    );
}

/// The rows on screen and the selected one, none while the popup is down.
fn bench_rows(session: &Rc<Session>) -> (Vec<String>, Option<u32>) {
    match session.is_shown() {
        true => session.rows(),
        false => (Vec::new(), None),
    }
}

/// One character as a key press types it: the chain first (a bracket's pair is `typing`'s),
/// the character itself where nothing in the chain took the key.
fn bench_char(tab: &Rc<Tab>, c: char) {
    let name = match c {
        '[' => "bracketleft".to_string(),
        '#' => "numbersign".to_string(),
        '.' => "period".to_string(),
        '-' => "minus".to_string(),
        c => c.to_string(),
    };
    let none = gdk::ModifierType::empty();
    let taken = gdk::Key::from_name(&name)
        .is_some_and(|key| editor::press(tab, key, none) == glib::Propagation::Stop);
    if !taken {
        tab.buffer.insert_at_cursor(&c.to_string());
    }
}

/// A key through the chain, and what the popup and the caret's line are after it.
async fn bench_key(tab: &Rc<Tab>, session: &Rc<Session>, step: &str, key: gdk::Key) {
    let answer = editor::press(tab, key, gdk::ModifierType::empty());
    bench_settle(session).await;
    let line = editor::caret(&tab.buffer).line();
    println!(
        "bench {step} {answer:?} {:?} selected={:?} shown={}",
        bench_line(tab, line),
        bench_rows(session).1,
        session.is_shown()
    );
}

/// Until nothing is asked or waited for, and a frame on.
async fn bench_settle(session: &Rc<Session>) {
    glib::timeout_future(Duration::from_millis(30)).await;
    for _ in 0..500 {
        if session.settled() {
            break;
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    glib::timeout_future(Duration::from_millis(30)).await;
}

fn bench_line(tab: &Rc<Tab>, line: i32) -> String {
    let Some(start) = tab.buffer.iter_at_line(line) else {
        return String::new();
    };
    tab.buffer
        .text(&start, &editor::line_end(&tab.buffer, line), true)
        .to_string()
}
