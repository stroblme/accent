//! Persistent file-state questions shown in the tab's banner.

use super::Tab;

/// Store banner intent so its button always performs the labelled action,
/// even if disk state changes before the click. The first two variants
/// require unsaved edits; clean tabs reload silently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alert {
    /// External edits conflict with this buffer. Compare before discarding
    /// either version.
    Compare,
    /// The file is gone and this buffer is the only copy left. The button writes it back.
    Restore,
    /// A sibling `*.sync-conflict-*` copy exists. Open the tree's side-by-side
    /// resolver using the vault-reported copy.
    Conflict,
    /// Invalid UTF-8 displayed lossily. Keep the tab read-only with no action
    /// button to preserve the original bytes.
    ReadOnly,
    /// A file a package manager or a toolchain installed, outside the vault, read-only with no
    /// action button: it is theirs, not the vault's to edit.
    Installed,
}

impl Alert {
    fn title(self) -> &'static str {
        match self {
            Alert::Compare => "This file changed on disk",
            Alert::Restore => "This file was deleted on disk",
            Alert::Conflict => "A sync conflict copy of this file exists",
            Alert::ReadOnly => "This file is not valid UTF-8 and is shown read-only",
            Alert::Installed => "This file belongs to an installed package and is shown read-only",
        }
    }

    /// No action for informational states or while the button's comparison
    /// is already shown, preventing duplicate comparisons.
    fn button(self, shown: Option<Alert>) -> Option<&'static str> {
        if shown == Some(self) {
            return None;
        }
        match self {
            Alert::Compare => Some("Compare"),
            Alert::Restore => Some("Save"),
            Alert::Conflict => Some("Resolve"),
            Alert::ReadOnly | Alert::Installed => None,
        }
    }
}

/// Choose the highest-priority standing question for the tab's single banner.
///
/// Queue questions so resolving one cannot erase another. Unsaved-copy risks lead, conflict
/// copies follow, and read-only status is last. Separate queued questions preserve one
/// meaningful action label because AdwBanner has only one button.
fn banner_alert(standing: &[Alert]) -> Option<Alert> {
    [
        Alert::Restore,
        Alert::Compare,
        Alert::Conflict,
        Alert::ReadOnly,
        Alert::Installed,
    ]
    .into_iter()
    .find(|a| standing.contains(a))
}

impl Tab {
    /// Queue `alert` and display the highest-priority pending question.
    pub fn show_alert(&self, alert: Alert) {
        let mut standing = self.alerts.borrow_mut();
        if !standing.contains(&alert) {
            standing.push(alert);
        }
        drop(standing);
        self.render_banner();
    }

    /// Remove one question and display the next, preserving other pending states.
    pub fn clear_alert(&self, alert: Alert) {
        self.alerts.borrow_mut().retain(|a| *a != alert);
        self.render_banner();
    }

    /// What the visible banner is asking for, for the handler of its button.
    pub fn alert(&self) -> Option<Alert> {
        banner_alert(&self.alerts.borrow())
    }

    pub fn hide_banner(&self) {
        self.alerts.borrow_mut().clear();
        self.render_banner();
    }

    pub(super) fn render_banner(&self) {
        match self.alert() {
            Some(alert) => {
                let shown = self.comparing.borrow().as_ref().and_then(|c| c.answers);
                self.banner.set_title(alert.title());
                self.banner.set_button_label(alert.button(shown));
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
    }

    /// Hide the `alert` button while its comparison is shown.
    pub fn comparing_answers(&self, alert: Alert) {
        if let Some(comparing) = self.comparing.borrow_mut().as_mut() {
            comparing.answers = Some(alert);
        }
        self.render_banner();
    }

    /// Clear changed/deleted-on-disk questions after save or reload. Keep
    /// sync-conflict questions so autosave cannot dismiss them.
    pub fn clear_disk_alert(&self) {
        self.alerts
            .borrow_mut()
            .retain(|a| !matches!(a, Alert::Compare | Alert::Restore));
        self.render_banner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One banner, more than one thing to say: they queue by what each can cost instead of
    /// overwriting each other.
    #[test]
    fn the_banner_shows_the_costliest_standing_question() {
        assert_eq!(banner_alert(&[]), None);
        assert_eq!(
            banner_alert(&[Alert::Conflict, Alert::Compare]),
            Some(Alert::Compare),
            "unsaved edits over a moved file outrank a copy sitting beside the note"
        );
        assert_eq!(
            banner_alert(&[Alert::Compare, Alert::Restore]),
            Some(Alert::Restore)
        );
        assert_eq!(
            banner_alert(&[Alert::ReadOnly, Alert::Conflict]),
            Some(Alert::Conflict),
            "a report never displaces a question"
        );
        assert_eq!(banner_alert(&[Alert::ReadOnly]), Some(Alert::ReadOnly));
    }

    /// Pressing Compare while the comparison it opens is on screen only built it again.
    #[test]
    fn the_button_goes_while_the_tab_shows_what_it_opens() {
        assert_eq!(Alert::Compare.button(None), Some("Compare"));
        assert_eq!(Alert::Compare.button(Some(Alert::Compare)), None);
        assert_eq!(
            Alert::Compare.button(Some(Alert::Conflict)),
            Some("Compare"),
            "a conflict being resolved is not the disk being compared"
        );
    }
}
