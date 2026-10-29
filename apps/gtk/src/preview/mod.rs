//! A markdown note's rendered preview, one type over its two renderers while WebKit's is retired:
//! the native one ([`native::Native`]), and WebKit's ([`crate::web::Preview`]) in a build with the
//! `webkit` feature when `ACCENT_PREVIEW=webkit` asks for it.

mod code;
mod figure;
pub(crate) mod find;
mod formula;
mod native;
mod table;
mod text;
mod view;

#[cfg(feature = "bench")]
pub use figure::Probe;

use accent_core::config::Config;
use accent_core::search::Options;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

/// How long a preview out of sight is kept, a WebKit one with its 60–120 MB process.
/// Keep short toggles and successive formulas warm; ACCENT_BENCH_WEBIDLE uses two seconds.
pub fn idle_for() -> std::time::Duration {
    #[cfg(feature = "bench")]
    if std::env::var_os("ACCENT_BENCH_WEBIDLE").is_some() {
        return std::time::Duration::from_secs(2);
    }
    std::time::Duration::from_secs(5 * 60)
}

/// The window's image keys a preview reads: the images inverted, the diagrams whose pictures on
/// the web are drawn.
type Keys = Rc<RefCell<HashSet<String>>>;

pub enum Preview {
    Native(native::Native),
    #[cfg(feature = "webkit")]
    Web(crate::web::Preview),
}

/// `$body` with `$p` bound to whichever renderer `$preview` is.
macro_rules! each {
    ($preview:expr, $p:ident => $body:expr) => {
        match $preview {
            Preview::Native($p) => $body,
            #[cfg(feature = "webkit")]
            Preview::Web($p) => $body,
        }
    };
}

impl Preview {
    /// A preview resolving assets through `resolve` and following links through `on_open`, tags
    /// through `on_tag` and Invert Image Colours through `on_invert`; see [`crate::web::Preview::new`].
    pub fn new(
        resolve: impl Fn(&str) -> Option<(String, PathBuf)> + Send + Sync + 'static,
        inverted: Keys,
        web: Keys,
        on_open: impl Fn(&str) + 'static,
        on_tag: impl Fn(&str) + 'static,
        on_invert: impl Fn(&str) + 'static,
    ) -> Preview {
        #[cfg(feature = "webkit")]
        if std::env::var_os("ACCENT_PREVIEW").is_some_and(|v| v == "webkit") {
            return Preview::Web(crate::web::Preview::new(
                resolve, inverted, web, on_open, on_tag, on_invert,
            ));
        }
        Preview::Native(native::Native::new(
            resolve, inverted, web, on_open, on_tag, on_invert,
        ))
    }

    /// What the window puts in a tab or lays over a pane.
    pub fn widget(&self) -> &gtk::Widget {
        each!(self, p => p.widget())
    }

    /// What takes the keyboard, which scrolls the page with the reading keys.
    pub fn focus_target(&self) -> &gtk::Widget {
        match self {
            Preview::Native(p) => p.focus_target(),
            #[cfg(feature = "webkit")]
            Preview::Web(p) => p.widget(),
        }
    }

    pub fn render(&self, rel: &str, text: &str) {
        each!(self, p => p.render(rel, text))
    }

    pub fn scroll_to_line(&self, line: u32) {
        each!(self, p => p.scroll_to_line(line))
    }

    pub fn restyle(&self) -> bool {
        each!(self, p => p.restyle())
    }

    pub fn forget_images(&self, then: impl FnOnce() + 'static) {
        each!(self, p => p.forget_images(then))
    }

    pub fn forget_image(&self, key: &str) -> bool {
        each!(self, p => p.forget_image(key))
    }

    pub fn connect_lost(&self, f: impl Fn() + 'static) {
        each!(self, p => p.connect_lost(f))
    }

    pub fn set_zoom(&self, zoom: f64) {
        each!(self, p => p.set_zoom(zoom))
    }

    pub fn set_column(&self, config: &Config) {
        each!(self, p => p.set_column(config))
    }

    pub fn find(&self, text: &str, options: Options) {
        each!(self, p => p.find(text, options))
    }

    pub fn find_next(&self) {
        each!(self, p => p.find_next())
    }

    pub fn find_previous(&self) {
        each!(self, p => p.find_previous())
    }

    pub fn find_clear(&self) {
        each!(self, p => p.find_clear())
    }

    pub fn connect_found(&self, f: impl Fn(&str) + 'static) {
        each!(self, p => p.connect_found(f))
    }

    /// The native preview, for a drill that reads its page.
    #[cfg(feature = "bench")]
    pub fn native(&self) -> Option<&native::Native> {
        match self {
            Preview::Native(p) => Some(p),
            #[cfg(feature = "webkit")]
            Preview::Web(_) => None,
        }
    }

    /// The WebKit preview, for a drill that reads its page.
    #[cfg(feature = "bench")]
    pub fn web(&self) -> Option<&crate::web::Preview> {
        match self {
            Preview::Web(p) => Some(p),
            Preview::Native(_) => None,
        }
    }
}
