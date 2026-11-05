//! Starting a tab's shell, once its host is ready and its view has settled on a size.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use accent_api::ssh;
use gtk::prelude::*;
use gtk::{gio, glib};
use vte4::TerminalExt;
use vte4::TerminalExtManual;

use super::Term;
use super::holder::{end_orphans, output, told};
use super::key::id;
use super::shell::{Run, Shell, command};

/// Start locally at once, or after [`accent_api::link::prepare`] installs the remote server.
/// Shell-only windows show preparation progress in-place in the tab because they have no connect bar.
pub(super) fn start(term: &Rc<Term>) {
    let Shell::Remote { link, .. } = &term.shell else {
        name_unshown(term);
        return spawn(term);
    };
    term.view
        .feed(format!("Connecting to {}…", link.host).as_bytes());
    let (link, weak) = (link.clone(), Rc::downgrade(term));
    let ctl = ssh::control_path(&link);
    let view = glib::SendWeakRef::from(term.view.downgrade());
    // Set once there is an outcome. A line the worker said just before it returned can reach the
    // main loop after that, and would be drawn over the shell's first prompt.
    let over = Arc::new(AtomicBool::new(false));
    glib::spawn_future_local(async move {
        let what = format!("connect to {}", link.host);
        let listening = over.clone();
        let ready = crate::work::attempt(&what, move || {
            let say = |line: &str, _: Option<f64>| progress(&view, &listening, line);
            accent_api::link::prepare(&link, &ssh::control_path(&link), false, &say)
        })
        .await;
        over.store(true, Ordering::Relaxed);
        if ready.is_ok() {
            end_orphans(&ctl);
        }
        let Some(term) = weak.upgrade() else {
            return;
        };
        term.view.feed(CLEAR_LINE);
        match ready {
            Ok(()) => {
                name_unshown(&term);
                spawn(&term)
            }
            Err(why) => {
                term.lost.set(true);
                let line = format!("[{why}. Press a key to try again.]");
                term.view.feed(line.replace('\n', "\r\n").as_bytes());
            }
        }
    });
}

/// Title a tab whose attach waits until it is first shown (a restored one behind another) with
/// the title its shell last set, which the holder keeps: one `held` listing, where attaching would
/// replay a screen at a size the tab does not have yet ([`when_laid_out`]).
///
/// Checked from an idle: a new tab is put in front right after it opens, so it is mapped by then
/// and asks nothing. Not through [`Term::ask`], whose coalescing would drop the Details pane's own
/// callback.
fn name_unshown(term: &Rc<Term>) {
    let weak = Rc::downgrade(term);
    glib::idle_add_local_once(move || {
        let Some(term) = weak
            .upgrade()
            .filter(|term| !term.view.is_mapped() && term.run.is_none())
        else {
            return;
        };
        let Some(argv) = term.shell.held() else {
            return;
        };
        let (weak, id) = (Rc::downgrade(&term), id(&term.key).to_string());
        glib::spawn_future_local(async move {
            let listing = crate::work::attempt("list the held shells", move || output(&argv)).await;
            let Some(term) = weak.upgrade() else {
                return;
            };
            let title = listing
                .ok()
                .and_then(|listing| told(&String::from_utf8_lossy(&listing), &id).title);
            // VTE's own wins: the tab may have been shown, and its replay have landed, meanwhile.
            if let Some(title) =
                title.filter(|_| term.view.window_title().is_none_or(|t| t.is_empty()))
            {
                term.page.set_title(&title);
            }
        });
    });
}

/// Back to the start of the line, and the line cleared.
const CLEAR_LINE: &[u8] = b"\r\x1b[2K";

/// Draw one of [`start`]'s progress lines over the last, from the worker making the host ready:
/// through the main loop, as `pdf::render` hands a page back, and not once `over` is set.
fn progress(view: &glib::SendWeakRef<vte4::Terminal>, over: &Arc<AtomicBool>, line: &str) {
    let (view, over, line) = (view.clone(), over.clone(), format!("{line}…"));
    glib::idle_add_once(move || {
        if let Some(view) = view.upgrade().filter(|_| !over.load(Ordering::Relaxed)) {
            view.feed(CLEAR_LINE);
            view.feed(line.as_bytes());
        }
    });
}

/// Run `shell` in `view` after any previous child exits.
///
/// `accent-cli attach` starts or resumes shell `id` in the holder so it survives tab ptys and
/// window closes. Without `accent-cli`, the shell runs directly and ends with the tab.
///
/// Wait for [`when_laid_out`] before attaching: replay at VTE's default or unsettled size
/// persists until the program redraws. Restored background tabs attach when first shown;
/// new tabs wait for two stable frames. A debugged program behind another tab starts at once, at
/// VTE's default size, as the adapter is waiting for it, and so does a command taken up again, so
/// that one which has ended closes its tab before anyone looks for it there.
fn spawn(term: &Term) {
    let (view, shell, id) = (&term.view, term.shell.clone(), id(&term.key).to_string());
    if let Shell::Console = shell {
        return;
    }
    let flags = term.run.as_ref().map(Run::flags).unwrap_or_default();
    let program = match &shell {
        Shell::Program(program) => Some(program.clone()),
        _ => None,
    };
    let resumed = term.run.as_ref().is_some_and(|run| run.resumed.get());
    let behind = (program.is_some() || resumed) && !term.page.is_selected();
    let run = move |view: &vte4::Terminal| {
        // Made as it runs: a tab attached once first shown names the server on the host then.
        let Some((cwd, argv, env)) = command(&shell, &id, &flags) else {
            return;
        };
        let named = argv.join(" ");
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        // VTE adds these to the environment the tab would have anyway.
        let env: Vec<&str> = env.iter().map(String::as_str).collect();
        view.spawn_async(
            vte4::PtyFlags::DEFAULT,
            cwd.as_deref().and_then(Path::to_str),
            &args,
            &env,
            // A remote shell's `ssh` from `$PATH`, as the link's own is: without it VTE looks in
            // `/bin:/usr/bin` only, and an ssh installed elsewhere ran the master but not the tab.
            glib::SpawnFlags::SEARCH_PATH,
            || {},
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Some(program) = program {
                    return (program.started)(match result {
                        Ok(pid) if program.link.is_none() => Ok(Some(pid.0 as u32)),
                        Ok(_) => Ok(None),
                        Err(e) => Err(e.to_string()),
                    });
                }
                if let Err(e) = result {
                    tracing::warn!("cannot start {named}: {e}");
                }
            },
        );
    };
    match behind {
        true => run(view),
        false => when_laid_out(view, term.run.is_some(), run),
    }
}

/// Run `then` once `view` is mapped with a stable allocation, or immediately if already laid out.
///
/// Two equal-sized frames let restored split ratios settle before shell replay. GTK4 has no
/// allocation signal, so mapped-only ticks detect stability without work on background tabs.
/// The callback receives the view rather than capturing it, avoiding a reference cycle.
/// `eager` runs it at once should the view go off screen first, as a user's command's tab does
/// when another is opened in front of it straight away: it has a command to start, not a prompt.
fn when_laid_out(view: &vte4::Terminal, eager: bool, then: impl FnOnce(&vte4::Terminal) + 'static) {
    if view.is_mapped() && view.height() > 0 {
        return then(view);
    }
    let then = Rc::new(Cell::new(Some(then)));
    if eager {
        let then = then.clone();
        view.connect_unmap(move |view| {
            if let Some(then) = then.take() {
                then(view);
            }
        });
    }
    let wait = move |view: &vte4::Terminal| {
        let (then, last) = (then.clone(), Cell::new((0, 0)));
        view.add_tick_callback(move |view, _| {
            // Hidden again first: the next map waits afresh.
            if !view.is_mapped() {
                return glib::ControlFlow::Break;
            }
            let size = (view.width(), view.height());
            if size.1 == 0 || last.replace(size) != size {
                return glib::ControlFlow::Continue;
            }
            if let Some(then) = then.take() {
                then(view);
            }
            glib::ControlFlow::Break
        });
    };
    if view.is_mapped() {
        wait(view);
    }
    view.connect_map(wait);
}
