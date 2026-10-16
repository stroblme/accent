//! Fill the Info pane's Details from the active document's cached facts or one disk stat.
//! Shells use their holder's facts instead.
//!
//! Created time is omitted: remote musl lacks birth time, and atomic saves, Syncthing and
//! other editors replace inodes, making available birth times describe the last replacement.

use super::*;
use sidebar::{Fact, Group};

impl App {
    /// Show the facts of the file in front in the Details section, while it is on screen. Never
    /// touches the disk: what only a stat can say comes from [`App::stat_details`].
    pub(crate) fn sync_details(&self) {
        let Some(sidebar) = self.sidebar.get() else {
            return;
        };
        if !sidebar.section_live("details") {
            return;
        }
        if let Some(Doc::Terminal(term)) = self.active_doc() {
            return sidebar.set_details(&[shell_group(term.facts())]);
        }
        let Some(doc) = self.active_doc().filter(|doc| !doc.is_transient()) else {
            return sidebar.set_details(&[]);
        };
        let etag = match &doc {
            Doc::Text(tab) => tab.save.etag.get(),
            Doc::Diagram(d) => d.save.etag.get(),
            _ => self
                .details_stat
                .borrow()
                .as_ref()
                .filter(|(key, _)| *key == doc.key())
                .and_then(|(_, etag)| *etag),
        };
        let mut groups = Vec::new();
        if let Some(etag) = etag {
            groups.push(Group {
                title: "File",
                facts: vec![
                    fact("Size", glib::format_size(etag.size)),
                    fact(
                        "Modified",
                        date_label(etag.mtime_ns.div_euclid(1_000_000_000)),
                    ),
                ],
            });
        }
        groups.extend(kind_group(&doc));
        sidebar.set_details(&groups);
    }

    /// Fetch missing file size and time once per file on a worker, then update Details.
    ///
    /// Remote stats cost a round trip. Shells instead query their holder every time because
    /// foreground commands can change without a file event.
    pub(crate) fn stat_details(self: &Rc<Self>) {
        if !self
            .sidebar
            .get()
            .is_some_and(|s| s.section_live("details"))
        {
            return;
        }
        if let Some(Doc::Terminal(term)) = self.active_doc() {
            let weak = Rc::downgrade(self);
            return term.ask(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.sync_details();
                }
            });
        }
        let Some(key) = self
            .active_doc()
            .filter(|doc| matches!(doc, Doc::Pdf(_) | Doc::Image(_) | Doc::Status(_)))
            .map(|doc| doc.key())
        else {
            return;
        };
        if self
            .details_stat
            .borrow()
            .as_ref()
            .is_some_and(|(asked, _)| *asked == key)
        {
            return;
        }
        // Taken at once, so the asks a busy moment makes are one stat; a file switched to
        // meanwhile takes the slot and this answer is dropped.
        *self.details_stat.borrow_mut() = Some((key.clone(), None));
        let vault = self.vault().filter(|_| !doc::is_loose_key(&key)).cloned();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let asked = key.clone();
            let etag = work::off_thread("stat", move || match vault {
                Some(vault) => vault.stat(&asked).ok().flatten(),
                None => Etag::of(Path::new(&asked)).ok(),
            })
            .await
            .flatten();
            let Some(app) = weak.upgrade() else { return };
            let mut stat = app.details_stat.borrow_mut();
            if stat.as_ref().is_some_and(|(asked, _)| *asked == key) {
                *stat = Some((key, etag));
                drop(stat);
                app.sync_details();
            }
        });
    }

    /// The file `key` was written, by accent or anyone else: its stat is asked again.
    pub(crate) fn details_stale(self: &Rc<Self>, key: &str) {
        if self
            .details_stat
            .borrow()
            .as_ref()
            .is_some_and(|(asked, _)| asked == key)
        {
            self.details_stat.take();
        }
        self.stat_details();
    }
}

fn fact(name: &'static str, value: impl ToString) -> Fact {
    Fact {
        name,
        value: value.to_string(),
    }
}

/// What the file's kind adds, read off the open tab.
fn kind_group(doc: &Doc) -> Option<Group> {
    let (title, facts) = match doc {
        Doc::Text(tab) => {
            let lines = lines(&tab.buffer);
            let characters = tab.buffer.char_count();
            match tab.flavour() {
                editor::Flavour::Note => (
                    "Note",
                    vec![
                        fact("Words", tab.words()),
                        fact("Characters", characters),
                        fact("Lines", lines),
                        fact("Links", tab.link_count()),
                    ],
                ),
                editor::Flavour::Code => (
                    "Text",
                    vec![
                        fact(
                            "Language",
                            tab.language().unwrap_or_else(|| "Plain Text".to_string()),
                        ),
                        fact("Encoding", tab.encoding()),
                        fact("Line Endings", tab.line_ending()),
                        fact("Lines", lines),
                        fact("Characters", characters),
                    ],
                ),
                editor::Flavour::Csv(_) => (
                    "Table",
                    vec![
                        fact("Rows", lines),
                        fact("Encoding", tab.encoding()),
                        fact("Line Endings", tab.line_ending()),
                    ],
                ),
            }
        }
        // Nothing to say while it is still opening.
        Doc::Pdf(pdf) if pdf.page_count() > 0 => {
            let mut facts = vec![fact("Pages", pdf.page_count())];
            facts.extend(
                pdf.page_size(0)
                    .map(|size| fact("Page Size", paper_label(size))),
            );
            let info = pdf.info();
            let said = [
                ("Title", info.title),
                ("Author", info.author),
                ("Subject", info.subject),
                ("Keywords", info.keywords),
                ("Application", info.creator),
                ("Producer", info.producer),
                ("Created", info.created.map(date_label)),
                ("PDF Version", info.version),
            ];
            facts.extend(
                said.into_iter()
                    .filter_map(|(name, value)| Some(fact(name, value?))),
            );
            ("PDF", facts)
        }
        Doc::Image(image) => {
            let mut facts: Vec<Fact> = image
                .size
                .get()
                .map(|(w, h)| fact("Dimensions", format!("{w} × {h}")))
                .into_iter()
                .collect();
            if let Some((_, ext)) = image.key().rsplit_once('.') {
                facts.push(fact("Format", ext.to_uppercase()));
            }
            ("Image", facts)
        }
        Doc::Diagram(d) => ("Diagram", vec![fact("Pages", d.page_count())]),
        _ => return None,
    };
    Some(Group { title, facts })
}

/// What the shell in front is, each row only where it is known, and Running only while a command
/// is.
fn shell_group(facts: terminal::Facts) -> Group {
    let said = [
        ("Shell", facts.program),
        ("PID", facts.pid.map(|pid| pid.to_string())),
        ("Host", Some(facts.host)),
        ("Directory", facts.dir),
        ("Running", facts.running),
        ("Started", facts.started.map(date_label)),
    ];
    Group {
        title: "Terminal",
        facts: said
            .into_iter()
            .filter_map(|(name, value)| Some(fact(name, value?)))
            .collect(),
    }
}

/// The lines a file holds, as `wc -l` counts them: the empty line after a last newline, which
/// the buffer counts, is none.
fn lines(buffer: &sourceview5::Buffer) -> i32 {
    let ends_open = buffer.char_count() > 0 && buffer.end_iter().starts_line();
    buffer.line_count() - i32::from(ends_open)
}

/// A time, in seconds since the epoch, as the locale writes a date and a time.
fn date_label(secs: i64) -> String {
    glib::DateTime::from_unix_local(secs)
        .and_then(|time| time.format("%x %X"))
        .map(|text| text.to_string())
        .unwrap_or_default()
}

/// A page size in points as the paper it is — "A4", "US Letter" — either way round, else in
/// millimetres.
fn paper_label((width, height): (f32, f32)) -> String {
    let near = |a: f64, b: f32| (a - f64::from(b)).abs() <= 1.0;
    gtk::PaperSize::paper_sizes(false)
        .into_iter()
        .find(|paper| {
            let (w, h) = (
                paper.width(gtk::Unit::Points),
                paper.height(gtk::Unit::Points),
            );
            (near(w, width) && near(h, height)) || (near(w, height) && near(h, width))
        })
        .map(|paper| paper.display_name().to_string())
        .unwrap_or_else(|| {
            let mm = |points: f32| (points * 25.4 / 72.0).round();
            format!("{} × {} mm", mm(width), mm(height))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shell at its prompt has no Running row, and one the holder has not answered for yet
    /// shows what its tab knows.
    #[test]
    fn a_shell_lists_what_is_known_of_it() {
        let names =
            |facts| -> Vec<&str> { shell_group(facts).facts.iter().map(|f| f.name).collect() };
        let tab = terminal::Facts {
            host: "box".into(),
            dir: Some("/home/me".into()),
            ..Default::default()
        };
        assert_eq!(names(tab.clone()), ["Host", "Directory"]);
        let told = terminal::Facts {
            program: Some("/bin/zsh".into()),
            pid: Some(42),
            running: Some("cargo build".into()),
            started: Some(1_760_000_000),
            ..tab
        };
        assert_eq!(
            names(told.clone()),
            ["Shell", "PID", "Host", "Directory", "Running", "Started"]
        );
        let facts = shell_group(told).facts;
        assert_eq!(facts[4].value, "cargo build");
    }
}
