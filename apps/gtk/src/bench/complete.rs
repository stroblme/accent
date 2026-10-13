//! Exercise real note and language-server completions through the editor key path.

use super::*;
use crate::completion::{self, Session};

/// `=note:<rel>` and `=code:<rel>[,<typed>]` type into files; use a scratch vault under `/tmp`.
///
/// `note:` creates completion fixtures, narrows `[[` and `#c` suggestions, accepts
/// with Down/Tab and undoes. Bracket completion must consume the existing `]]` once.
/// `code:` types `typed` (default `s.g`) into a blank line, creating a C fixture if
/// absent. Print rows, selected documentation, accepted imports and undo results, then type it
/// again and commit the first row with `;`.
/// Requests from `x` and `.` 50 ms apart test cancellation (`fake-lsp cancelled` with
/// `RUST_LOG=accent_lsp::stderr=debug`). `build-aux/fake-lsp.py` gives fixed output for
/// `make drills`; real servers can exercise imports for unimported symbols.
/// Each step prints `time complete …` latency from keystroke to rows.
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
    bench_keys(&tab, "x").await;
    glib::timeout_future(Duration::from_millis(50)).await;
    bench_type(&tab, &session, "complete_code_superseded", ".").await;

    // A commit character: the row the arrows picked goes in as its look resolved it, and the
    // character after it.
    tab.set_text(&text);
    body();
    bench_keys(&tab, typed).await;
    bench_settle(&session).await;
    bench_key(&tab, &session, "complete_commit_down", gdk::Key::Down).await;
    bench_type(&tab, &session, "complete_commit_;", ";").await;
    let line = editor::caret(&tab.buffer).line();
    println!(
        "bench complete_commit_accepted {:?} {:?}",
        bench_line(&tab, 0),
        bench_line(&tab, line)
    );

    // `.g` in one burst while `s` is being answered, both landing before the look at the `.`:
    // the trigger still asks, or the `s` answer closes the popup.
    tab.set_text(&text);
    body();
    bench_keys(&tab, "s").await;
    bench_char(&tab, '.');
    bench_char(&tab, 'g');
    bench_settle(&session).await;
    println!(
        "bench complete_code_burst {:?} shown={}",
        bench_rows(&session).0,
        session.is_shown()
    );

    // A row committed before its look has resolved it, the typing going on: the `#include`
    // the resolve keeps back follows the row in once it lands.
    tab.set_text(&text);
    body();
    bench_keys(&tab, typed).await;
    bench_settle(&session).await;
    editor::press(&tab, gdk::Key::Down, gdk::ModifierType::empty());
    bench_char(&tab, ';');
    bench_keys(&tab, " ").await;
    let line = editor::caret(&tab.buffer).line();
    println!(
        "bench complete_late_committed {:?} {:?}",
        bench_line(&tab, 0),
        bench_line(&tab, line)
    );
    bench_settle(&session).await;
    let line = editor::caret(&tab.buffer).line();
    println!(
        "bench complete_late_resolved {:?} {:?}",
        bench_line(&tab, 0),
        bench_line(&tab, line)
    );

    // A snippet row accepted before its resolve, rust-analyzer's `foo(${1:a})` and its `use`:
    // the import waits while the stops are walked, the placeholder staying selected to be typed
    // over, and lands once the walk ends. A second is past the resolve's 0.3 s.
    tab.set_text(&text);
    body();
    bench_keys(&tab, "s.a").await;
    bench_settle(&session).await;
    editor::press(&tab, gdk::Key::Down, gdk::ModifierType::empty());
    editor::press(&tab, gdk::Key::Return, gdk::ModifierType::empty());
    bench_snippet(&tab, "accepted");
    glib::timeout_future(Duration::from_secs(1)).await;
    bench_snippet(&tab, "walked");
    tab.end_snippet();
    bench_settle(&session).await;
    bench_snippet(&tab, "resolved");

    // typescript-language-server's plain stop, `beta($1)`: the walk lands between the parens.
    tab.set_text(&text);
    body();
    bench_keys(&tab, "s.b").await;
    bench_settle(&session).await;
    editor::press(&tab, gdk::Key::Down, gdk::ModifierType::empty());
    editor::press(&tab, gdk::Key::Return, gdk::ModifierType::empty());
    bench_snippet(&tab, "plain");
    tab.end_snippet();
    bench_settle(&session).await;
    tab.set_text(&text);
    if let Err(e) = app.write_tab(&tab, None) {
        println!("bench complete write_failed {e}");
    }
}

/// The first line and the caret's, the caret marked `|`, what is selected, and whether a snippet
/// is being walked.
fn bench_snippet(tab: &Rc<Tab>, step: &str) {
    let caret = editor::caret(&tab.buffer);
    let line = bench_line(tab, caret.line());
    let (before, after) = line.split_at(
        line.char_indices()
            .nth(caret.line_offset() as usize)
            .map_or(line.len(), |(i, _)| i),
    );
    let selected = tab
        .buffer
        .selection_bounds()
        .map(|(a, b)| a.slice(&b).to_string());
    println!(
        "bench complete_snippet_{step} {:?} \"{before}|{after}\" selected={selected:?} walking={}",
        bench_line(tab, 0),
        tab.snippet_active()
    );
}

/// The tab at `rel` once it is open on the language layer, with the keyboard, and its session.
pub(super) async fn bench_complete_open(
    app: &Rc<App>,
    rel: &str,
) -> Option<(Rc<Tab>, Rc<Session>)> {
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
    bench_keys(tab, text).await;
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

/// Type `text` through the key chain a keystroke at a time, each looked at before the next, as
/// a main loop that keeps up with the typist has it. Keystrokes landing before the look at the
/// first, as a busy loop has them, are what `complete_code_burst` types on purpose.
async fn bench_keys(tab: &Rc<Tab>, text: &str) {
    for c in text.chars() {
        bench_char(tab, c);
        bench_idle().await;
    }
}

/// Until the idles queued before this one have run. `completion::install` looks at a keystroke
/// from an idle queued as it lands, and GLib runs idles of one priority in the order they were
/// queued, so whatever that look asked is in flight once this returns.
async fn bench_idle() {
    let (done, idle) = futures_channel::oneshot::channel();
    glib::idle_add_local_once(move || {
        let _ = done.send(());
    });
    let _ = idle.await;
}

/// Run a character through editor key handlers before inserting it if unhandled.
pub(super) fn bench_char(tab: &Rc<Tab>, c: char) {
    let name = match c {
        '[' => "bracketleft".to_string(),
        '#' => "numbersign".to_string(),
        '.' => "period".to_string(),
        '-' => "minus".to_string(),
        ';' => "semicolon".to_string(),
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

/// Wait for what was typed to be looked at, the requests that made, and one further frame
/// before probing settled output. A fixed wait for the look lost its ask under load.
pub(super) async fn bench_settle(session: &Rc<Session>) {
    bench_idle().await;
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
