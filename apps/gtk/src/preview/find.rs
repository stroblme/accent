//! Find in the preview. Stage 2 P6 searches the buffer with `accent_core::search`, table cells
//! included, and paints the matches near the view; until then it finds nothing.

use accent_core::search::Options;
use std::cell::RefCell;

type Found = Box<dyn Fn(&str)>;

#[derive(Default)]
pub(super) struct Find {
    /// Told the readout, "3 of 12", after each search or step.
    found: RefCell<Option<Found>>,
}

impl Find {
    /// No match until P6: the readout says nothing.
    pub fn find(&self, _text: &str, _options: Options) {
        if let Some(found) = self.found.borrow().as_ref() {
            found("");
        }
    }

    pub fn next(&self) {}

    pub fn previous(&self) {}

    pub fn clear(&self) {}

    pub fn connect_found(&self, f: impl Fn(&str) + 'static) {
        *self.found.borrow_mut() = Some(Box::new(f));
    }
}
