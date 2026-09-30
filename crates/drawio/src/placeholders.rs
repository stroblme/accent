//! What a page is shown among and when: the context a label with `placeholders="1"` fills its
//! `%pagenumber%`, `%pagecount%` and `%date%` from.

/// Where and when a page is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// The page's place in its file, from 0.
    pub page: usize,
    /// How many pages the file has.
    pub pages: usize,
    /// The time `%date%`, `%time%`, `%timestamp%` and `%date{mask}%` show; without it they are
    /// left as written.
    pub now: Option<Now>,
}

/// A lone page and no clock.
impl Default for Context {
    fn default() -> Context {
        Context {
            page: 0,
            pages: 1,
            now: None,
        }
    }
}

/// A moment and the local time zone it is read in; the toolkit reads its clock, the crate does
/// the calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    /// Milliseconds since the Unix epoch.
    pub unix_ms: i64,
    /// The local time zone's offset from UTC, in minutes east of it.
    pub offset_minutes: i32,
}
