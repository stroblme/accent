//! The preview of a build without WebKit (the `webkit` feature off): a status page where the
//! rendered note would be, as a PDF tab without libpdfium is one, and every call the window
//! makes of a preview answered with nothing.

use accent_core::config::Config;
use accent_core::search::Options;
use adw::prelude::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

/// How long a hidden preview is kept, as long as WebKit's, so the window treats both alike.
pub fn idle_for() -> std::time::Duration {
    std::time::Duration::from_secs(5 * 60)
}

type Keys = Rc<RefCell<HashSet<String>>>;

pub struct Preview {
    widget: gtk::Widget,
}

impl Preview {
    pub fn new(
        _resolve: impl Fn(&str) -> Option<(String, PathBuf)> + Send + Sync + 'static,
        _inverted: Keys,
        _web: Keys,
        _on_open: impl Fn(&str) + 'static,
        _on_tag: impl Fn(&str) + 'static,
        _on_invert: impl Fn(&str) + 'static,
    ) -> Preview {
        let page = adw::StatusPage::builder()
            .icon_name("x-office-document-symbolic")
            .title("Preview Is Not Available")
            .description("This build of accent has no WebKitGTK, which renders notes.")
            .build();
        Preview {
            widget: page.upcast(),
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        &self.widget
    }

    pub fn render(&self, _rel: &str, _text: &str) {}

    pub fn scroll_to_line(&self, _line: u32) {}

    pub fn restyle(&self) -> bool {
        false
    }

    pub fn forget_images(&self, then: impl FnOnce() + 'static) {
        then();
    }

    pub fn forget_image(&self, _key: &str) -> bool {
        false
    }

    pub fn connect_lost(&self, _f: impl Fn() + 'static) {}

    pub fn set_zoom(&self, _zoom: f64) {}

    pub fn set_column(&self, _config: &Config) {}

    pub fn find(&self, _text: &str, _options: Options) {}

    pub fn find_next(&self) {}

    pub fn find_previous(&self) {}

    pub fn find_clear(&self) {}

    pub fn connect_found(&self, _f: impl Fn(&str) + 'static) {}
}
