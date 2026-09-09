//! The dialog ssh asks its questions through.
//!
//! OpenSSH will not prompt on a terminal accent does not have: it spawns `$SSH_ASKPASS` with the
//! prompt as `argv[1]`, takes the answer from that program's stdout and reads a non-zero exit as a
//! cancel, and `SSH_ASKPASS_REQUIRE=force` makes it do so even where a terminal exists. The program
//! it spawns is accent itself, re-entered through `ACCENT_ASKPASS`: one binary to install and one
//! binary to upload to a remote, and the question is then asked in accent's own window instead of
//! by a second program the user never installed. The trap is `SSH_ASKPASS_PROMPT=confirm`, which
//! ssh sets when the answer it wants is `yes` or `no` — an unknown host key — rather than a secret:
//! answering that one with a password field hands ssh a passphrase where it waits for a word.

use crate::dialogs::alert;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

/// Set on the ssh process by whoever spawns it, and the only sign that this process is answering
/// ssh rather than opening a vault: argv here is ssh's, so there is nothing else to recognise.
const ASKPASS: &str = "ACCENT_ASKPASS";
/// What ssh puts in the environment when it wants a word rather than a secret.
const PROMPT_KIND: &str = "SSH_ASKPASS_PROMPT";
/// Not accent's application id, and `NON_UNIQUE` on top of it. Either alone would do — the flag
/// keeps this process from handing its arguments to a running accent, the id keeps the two from
/// being taken for one another at all — and both are cheap next to an askpass that silently
/// activates the editor instead of asking. The cost is a generic icon in the window switcher for
/// as long as the dialog is up, there being no desktop file under this id.
const APP_ID: &str = "io.github.stroblme.Accent.Askpass";
/// The response that answers; "cancel" is the other one in both dialogs.
const CONFIRM: &str = "confirm";
/// What ssh reads as a go-ahead for a confirmation, and what it reads as a refusal.
const YES: &str = "yes";
const NO: &str = "no";
/// What the compositor calls the window. The heading says what is being asked; the title says who
/// is asking, which is the question a dialog arriving out of nowhere actually raises.
const TITLE: &str = "accent";

/// Answer ssh's question, if this process was spawned to answer one.
///
/// `None` on every normal launch, which leaves `main` untouched. `Some` means ssh is waiting on our
/// stdout and nothing else may happen in this process.
pub fn maybe_run() -> Option<glib::ExitCode> {
    std::env::var_os(ASKPASS)?;
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Password:".to_string());
    let kind = std::env::var(PROMPT_KIND).ok();
    Some(ask(&prompt, confirming(kind.as_deref())))
}

/// Whether ssh wants `yes` or `no` instead of a secret, from `SSH_ASKPASS_PROMPT`.
fn confirming(prompt_kind: Option<&str>) -> bool {
    prompt_kind == Some("confirm")
}

/// Run a main loop long enough to ask `prompt`, then print the answer.
fn ask(prompt: &str, confirm: bool) -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    // Where the dialog leaves its answer. Nothing is written to stdout while GTK is still running:
    // that pipe is ssh's, and a warning or a panic on the way out must not land in it.
    let answer = Rc::new(RefCell::new(None::<String>));
    app.connect_activate({
        let (answer, prompt) = (answer.clone(), prompt.to_string());
        move |app| present(app, &prompt, confirm, &answer)
    });
    // Only the program name: argv[1] is ssh's prompt, and GApplication would take it for a file.
    let code = app.run_with_args(&["accent"]);
    if code != glib::ExitCode::SUCCESS {
        // GTK never started — no display, most likely. Print nothing: ssh reads a silent failure
        // as a cancel, which is what a question nobody can see amounts to.
        return code;
    }
    match answer.take() {
        // Never traced and never logged: this line is the secret itself.
        Some(answer) => {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{answer}");
            let _ = out.flush();
            glib::ExitCode::SUCCESS
        }
        None => glib::ExitCode::FAILURE,
    }
}

/// Put the question on screen and keep the loop alive until it is answered.
fn present(
    app: &adw::Application,
    prompt: &str,
    confirm: bool,
    answer: &Rc<RefCell<Option<String>>>,
) {
    let entry = (!confirm).then(secret_entry);
    let dialog = match &entry {
        Some(entry) => secret_dialog(prompt, entry),
        None => confirm_dialog(prompt),
    };
    // A parentless dialog is a window of its own, which is the point: there is no accent window
    // behind this one to host it, and ssh is blocked until the compositor puts it in front of
    // whatever the user was doing. It is not one of the application's windows either, so the hold
    // is what keeps the loop from ending the moment `activate` returns; letting go of it when the
    // answer is in is what ends it.
    let hold = app.hold();
    let answer = answer.clone();
    dialog.choose(
        None::<&gtk::Widget>,
        gio::Cancellable::NONE,
        move |response| {
            *answer.borrow_mut() = match (response == CONFIRM, &entry) {
                (true, Some(entry)) => Some(entry.text().to_string()),
                (true, None) => Some(YES.to_string()),
                // A refused confirmation is still an answer ssh wants to hear; a refused secret is
                // not, and the non-zero exit is what tells ssh to give up rather than retry.
                (false, None) => Some(NO.to_string()),
                (false, Some(_)) => None,
            };
            drop(hold);
        },
    );
}

/// The unknown host key. ssh accepts `yes`, and reads anything else as a refusal.
fn confirm_dialog(prompt: &str) -> adw::AlertDialog {
    let dialog = alert(
        "Continue Connecting?",
        prompt,
        &[
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            (CONFIRM, "Continue", adw::ResponseAppearance::Suggested),
        ],
        CONFIRM,
    );
    dialog.set_title(TITLE);
    dialog
}

/// A passphrase, a password or a one-time code — whatever ssh's prompt asks for, typed once.
fn secret_dialog(prompt: &str, entry: &gtk::PasswordEntry) -> adw::AlertDialog {
    let dialog = alert(
        "Authentication Required",
        prompt,
        &[
            ("cancel", "Cancel", adw::ResponseAppearance::Default),
            (CONFIRM, "Unlock", adw::ResponseAppearance::Suggested),
        ],
        CONFIRM,
    );
    dialog.set_extra_child(Some(entry));
    // There is one thing to do here, so the keyboard starts in the field rather than on a button.
    dialog.set_focus(Some(entry));
    dialog.set_title(TITLE);
    dialog
}

/// Return is Unlock, since the entry activates the default response. The peek icon stays: a
/// passphrase typed blind into a dialog ssh is already waiting on is a retry nobody enjoys.
fn secret_entry() -> gtk::PasswordEntry {
    gtk::PasswordEntry::builder()
        .show_peek_icon(true)
        .activates_default(true)
        .build()
}

#[cfg(test)]
mod tests {
    use super::confirming;

    #[test]
    fn confirm_is_the_only_confirming_prompt() {
        assert!(confirming(Some("confirm")));
        assert!(!confirming(Some("none")));
        assert!(!confirming(Some("")));
    }

    #[test]
    fn without_the_variable_ssh_wants_a_secret() {
        assert!(!confirming(None));
    }
}
