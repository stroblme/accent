//! A command for each template that names an `accent-target:`, `win.new-template-<path>`: New
//! from Template with that template picked, which Run Command lists as New and the template's
//! name and its chord, rebound from its row, runs (DESIGN.md, Notes and Rebinding).
//!
//! The action is named by the template's vault-relative path, so the chord `[shortcuts]` keeps
//! under that name follows the file. A template renamed or deleted takes its action with it, and
//! the chord left stored fires nothing: GTK passes a chord whose action the window lacks on.

use crate::{App, fileops, palette};
use accent_api::{Config, Event};
use accent_core::path::{parent_dir, stem};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::Path;
use std::rc::Rc;

/// What a template's action is called on the window, before its escaped path.
const NAME: &str = "new-template-";

/// The window action's own name for the template at `rel`. A `GAction` name holds only ASCII
/// letters, digits, `-` and `.`, so every other byte, `-` included, is written as `-` and two
/// hex digits: `Templates/Daily.md` is `new-template-Templates-2fDaily.md`, one name per path.
fn name(rel: &str) -> String {
    let mut name = NAME.to_string();
    for byte in rel.bytes() {
        match byte.is_ascii_alphanumeric() || byte == b'.' {
            true => name.push(char::from(byte)),
            false => name.push_str(&format!("-{byte:02x}")),
        }
    }
    name
}

/// The window action that makes a note from the template at `rel`.
pub fn action(rel: &str) -> String {
    format!("win.{}", name(rel))
}

/// The template a `win.new-template-…` action makes its note from: `None` for any other action,
/// and for a name [`action`] would not have written, such as a hand edit of `[shortcuts]`.
pub fn template(action: &str) -> Option<String> {
    let escaped = action.strip_prefix("win.")?.strip_prefix(NAME)?.as_bytes();
    let mut bytes = Vec::with_capacity(escaped.len());
    let mut at = 0;
    while let Some(&byte) = escaped.get(at) {
        at += 1;
        if byte != b'-' {
            bytes.push(byte);
            continue;
        }
        let hex = std::str::from_utf8(escaped.get(at..at + 2)?).ok()?;
        bytes.push(u8::from_str_radix(hex, 16).ok()?);
        at += 2;
    }
    let rel = String::from_utf8(bytes).ok()?;
    (self::action(&rel) == action).then_some(rel)
}

/// What the template's row says: New and the template's name.
pub fn label(rel: &str) -> String {
    format!("New {}", stem(rel))
}

/// Run Command's rows for the templates the window holds, after the user's commands, `used`
/// being the window's recently run commands.
pub fn items(config: &Config, templates: &[String], used: &[String]) -> Vec<palette::Item> {
    templates
        .iter()
        .map(|rel| {
            let action = action(rel);
            palette::Item::Command {
                label: label(rel),
                accels: crate::actions::accels_for(config, &action),
                recent: used.iter().position(|a| *a == action),
                action,
                why: None,
                listed: true,
            }
        })
        .collect()
}

/// Every template chord `[shortcuts]` holds, with the row holding it, which a rebind refuses.
pub fn taken(config: &Config) -> impl Iterator<Item = (String, String)> + '_ {
    config
        .shortcuts
        .iter()
        .filter_map(|(action, accels)| Some((label(&template(action)?), accels)))
        .flat_map(|(label, accels)| accels.iter().map(move |a| (a.clone(), label.clone())))
}

/// Whether `event` may have changed which templates in `dir` name a target: a file in it
/// written, removed or renamed, its listing changed, or it or a folder holding it removed or
/// renamed.
fn changes(dir: &str, event: &Event) -> bool {
    let touches =
        |rel: &str| parent_dir(rel) == dir || (!rel.is_empty() && Path::new(dir).starts_with(rel));
    match event {
        Event::DirsChanged(dirs) => dirs.iter().any(|d| d == dir),
        Event::FileChanged(rel) | Event::FileRemoved(rel) => touches(rel),
        Event::FileRenamed { from, to } => touches(from) || touches(to),
        _ => false,
    }
}

impl App {
    /// Ask again which templates name a target, off the main loop and on the host for a remote
    /// vault, as New from Template asks, and give the window an action for each. A vault that
    /// cannot answer, its link down, leaves the window the ones it has.
    pub(crate) fn refresh_templates(self: &Rc<Self>) {
        let Some(vault) = self.vault().cloned() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let listed = crate::work::off_thread("template list", move || vault.template_targets());
            if let (Some(Ok(templates)), Some(app)) = (listed.await, weak.upgrade()) {
                app.sync_templates(templates);
            }
        });
    }

    /// [`Self::refresh_templates`] where `event` may have changed them. Our own save of a template
    /// is no event, so the save is told as the change it is.
    pub(crate) fn follow_templates(self: &Rc<Self>, event: &Event) {
        let dir = self.vault().map(|vault| vault.config().templates_dir);
        if dir.is_some_and(|dir| changes(&dir, event)) {
            self.refresh_templates();
        }
    }

    /// Give the window an action for each of `templates`, and take away those of templates gone.
    fn sync_templates(self: &Rc<Self>, templates: Vec<String>) {
        let names: Vec<String> = templates.iter().map(|rel| name(rel)).collect();
        for held in self.window.list_actions() {
            if held.starts_with(NAME) && !names.iter().any(|name| name == held.as_str()) {
                self.window.remove_action(&held);
            }
        }
        for (rel, name) in templates.iter().zip(&names) {
            if self.window.lookup_action(name).is_some() {
                continue;
            }
            let run = gio::SimpleAction::new(name, None);
            let rel = rel.clone();
            run.connect_activate(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_, _| {
                    app.command_used(&action(&rel));
                    if let Some(ops) = app.need_ops("create a note from a template") {
                        fileops::note_from_template(ops, &rel);
                    }
                }
            ));
            self.window.add_action(&run);
        }
        *self.templates.borrow_mut() = templates;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_is_its_action_and_back() {
        assert_eq!(
            action("Templates/Daily.md"),
            "win.new-template-Templates-2fDaily.md"
        );
        for rel in [
            "Templates/Daily.md",
            "Templates/Tägliche Notiz_2.md",
            "Meta/Templates/a-b (c).md",
            "Daily.md",
        ] {
            let action = action(rel);
            assert!(gio::Action::name_is_valid(&action[4..]), "{action}");
            assert_eq!(template(&action).as_deref(), Some(rel));
        }
        // Only a name `action` writes is a template's: not unescaped, not in capitals, not cut.
        for other in [
            "win.new-template-Templates/Daily.md",
            "win.new-template-Templates-2FDaily.md",
            "win.new-template-Templates-2",
            "win.new-from-template",
            "win.command-0",
        ] {
            assert_eq!(template(other), None, "{other}");
        }
        assert_eq!(label("Templates/Inbox Note.md"), "New Inbox Note");
    }

    #[test]
    fn its_chord_is_taken_and_its_folder_watched() {
        let mut config = Config::default();
        let daily = action("Templates/Daily.md");
        config
            .shortcuts
            .insert(daily, vec!["<Control><Shift>m".into()]);
        config
            .shortcuts
            .insert("win.save".into(), vec!["<Control>w".into()]);
        let taken: Vec<_> = taken(&config).collect();
        assert_eq!(taken, [("<Control><Shift>m".into(), "New Daily".into())]);

        let dir = "Templates";
        let changed = |rel: &str| changes(dir, &Event::FileChanged(rel.into()));
        assert!(changed("Templates/Daily.md") && changed("Templates"));
        assert!(!changed("Daily/2026-11-15.md") && !changed("note.md"));
        assert!(changes(dir, &Event::DirsChanged(vec!["Templates".into()])));
        assert!(!changes(dir, &Event::DirsChanged(vec![String::new()])));
        let renamed = Event::FileRenamed {
            from: "Old".into(),
            to: "Templates".into(),
        };
        assert!(changes(dir, &renamed));
        assert!(changes("", &Event::FileRemoved("note.md".into())));
    }
}
