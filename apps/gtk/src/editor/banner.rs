//! The tab's banner: the standing questions about its file, and which of them is on screen.

use super::Tab;

/// Why a tab's banner is up. The intent is stored rather than re-derived when the button is
/// pressed, so the button always does what its label says: deriving it from the file system meant
/// a Reload that could arrive as a Save, and a Save that quietly reloaded.
///
/// The first two only ever appear on a tab with unsaved edits: a clean tab is reloaded silently.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alert {
    /// Someone else wrote the file while this buffer had edits. The button opens the diff, which
    /// is the only honest one-click answer: neither side can be thrown away unseen.
    Compare,
    /// The file is gone and this buffer is the only copy left. The button writes it back.
    Restore,
    /// Syncthing left a `*.sync-conflict-*` copy of this note beside it. The button opens the
    /// same side-by-side resolver the tree offers, on the copy the vault reports.
    Conflict,
    /// The bytes are not valid UTF-8, so what is on screen is a lossy reading of them. There is
    /// no button: the only safe answer is to leave the file alone, which is what the tab does.
    ReadOnly,
}

impl Alert {
    fn title(self) -> &'static str {
        match self {
            Alert::Compare => "This note changed on disk",
            Alert::Restore => "This note was deleted on disk",
            Alert::Conflict => "A sync conflict copy of this note exists",
            Alert::ReadOnly => "This file is not valid UTF-8 and is shown read-only",
        }
    }

    /// `None` for a banner that only reports, which DESIGN.md allows: a banner is a state that
    /// persists, and not every state has an answer.
    fn button(self) -> Option<&'static str> {
        match self {
            Alert::Compare => Some("Compare"),
            Alert::Restore => Some("Save"),
            Alert::Conflict => Some("Resolve"),
            Alert::ReadOnly => None,
        }
    }
}

/// Which of the standing questions the one banner shows.
///
/// A tab has one `AdwBanner` and can have more than one thing to say about its file, so they
/// queue instead of overwriting each other: a conflict copy appearing used to wipe the "changed
/// on disk" question, and resolving that copy then took the bar down with the wiped question
/// still standing — a dirty tab that would never autosave again, with nothing on screen to say
/// why. The order is what each one can cost: the two that mean this buffer holds the only copy
/// of something come first, the conflict copy beside the note next (it blocks nothing), and the
/// read-only report last, because it asks nothing at all.
///
/// Queued rather than merged: `AdwBanner` has exactly one button, and two questions on one line
/// have no honest single label.
fn banner_alert(standing: &[Alert]) -> Option<Alert> {
    [
        Alert::Restore,
        Alert::Compare,
        Alert::Conflict,
        Alert::ReadOnly,
    ]
    .into_iter()
    .find(|a| standing.contains(a))
}

impl Tab {
    /// Raise `alert`, which decides both what the banner says and what its button does. It goes
    /// on the queue: whichever standing question matters most is the one on screen.
    pub fn show_alert(&self, alert: Alert) {
        let mut standing = self.alerts.borrow_mut();
        if !standing.contains(&alert) {
            standing.push(alert);
        }
        drop(standing);
        self.render_banner();
    }

    /// Take one question down, leaving whatever else is standing. The banner comes back with the
    /// next one rather than going away.
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

    fn render_banner(&self) {
        match self.alert() {
            Some(alert) => {
                self.banner.set_title(alert.title());
                self.banner.set_button_label(alert.button());
                self.banner.set_revealed(true);
            }
            None => self.banner.set_revealed(false),
        }
    }

    /// Take down the questions about the file on disk, and only those. A save or a reload answers
    /// "changed on disk" and "deleted on disk"; it says nothing about a conflict copy sitting
    /// next to the note, whose banner has to survive the first autosave.
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
}
