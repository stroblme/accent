//! Answer OpenSSH prompts in an Accent dialog without a separate askpass executable.
//!
//! SSH re-enters this binary through `ACCENT_ASKPASS`, passes the prompt in `argv[1]` and reads
//! the answer from stdout; a non-zero exit cancels. `SSH_ASKPASS_REQUIRE=force` enables this
//! path even with a terminal. `SSH_ASKPASS_PROMPT=confirm` selects a yes/no host-key dialog
//! instead of a secret entry.

use crate::dialogs::{alert, choose};
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

/// Environment marker identifying an SSH askpass launch, since SSH owns the argument vector.
const ASKPASS: &str = "ACCENT_ASKPASS";
/// SSH environment key distinguishing yes/no confirmation from secret prompts.
const PROMPT_KIND: &str = "SSH_ASKPASS_PROMPT";
/// Separate application ID for askpass, also using `NON_UNIQUE` to avoid activating the editor.
/// There is no desktop file for this ID, so the window switcher uses a generic icon.
const APP_ID: &str = "io.github.stroblme.Accent.Askpass";
/// The response that answers; "cancel" is the other one in both dialogs.
const CONFIRM: &str = "confirm";
/// What ssh reads as a go-ahead for a confirmation, and what it reads as a refusal.
const YES: &str = "yes";
const NO: &str = "no";
/// Askpass window title matching the desktop application name.
/// The heading identifies the question; the title identifies its source.
const TITLE: &str = "Accent";

/// Answer an SSH askpass launch, or return `None` for a normal launch.
/// `Some` reserves this process for SSH's stdout response.
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
    choose(&dialog, None::<&gtk::Widget>, move |response| {
        *answer.borrow_mut() = match (response == CONFIRM, &entry) {
            (true, Some(entry)) => Some(entry.text().to_string()),
            (true, None) => Some(YES.to_string()),
            // A refused confirmation is still an answer ssh wants to hear; a refused secret is
            // not, and the non-zero exit is what tells ssh to give up rather than retry.
            (false, None) => Some(NO.to_string()),
            (false, Some(_)) => None,
        };
        drop(hold);
    });
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

/// Return activates Unlock; retain the peek icon so users can check a typed passphrase.
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
