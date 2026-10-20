//! One cell's widgets: its source in an editor view, the `[n]` its run was counted as, its outputs
//! under it, its buttons, and the strip below it that adds a cell after it. The tab wires them.

use accent_core::notebook::{Cell, CellId, Kind};
use adw::prelude::*;
use gtk::gio;

use super::output::Outputs;
use crate::editor::Flavour;

pub struct CellView {
    pub id: CellId,
    pub kind: Kind,
    /// The cell and its inserter, the unit the notebook's column holds.
    pub root: gtk::Box,
    /// The cell itself, outlined in the accent while selected.
    pub card: gtk::Box,
    pub view: sourceview5::View,
    pub buffer: sourceview5::Buffer,
    pub outputs: Outputs,
    pub tools: gtk::Box,
    pub up: gtk::Button,
    pub down: gtk::Button,
    pub delete: gtk::Button,
    pub more: gtk::MenuButton,
    pub inserter: gtk::Box,
    pub add_code: gtk::Button,
    pub add_markdown: gtk::Button,
}

impl CellView {
    /// `cell`'s widgets, a code cell's source coloured as `language`.
    pub fn new(cell: &Cell, language: Option<&sourceview5::Language>) -> CellView {
        let (flavour, language) = match cell.kind() {
            Kind::Code => (Flavour::Code, language.cloned()),
            Kind::Markdown => (Flavour::Note, None),
            Kind::Raw => (Flavour::Code, None),
        };
        let (view, buffer) = crate::editor::build(flavour, language, cell.source());
        // The notebook's one history records what is typed (`history.rs`).
        buffer.set_enable_undo(false);
        view.set_left_margin(8);
        view.set_right_margin(8);
        view.set_top_margin(6);
        view.set_bottom_margin(6);
        view.set_hexpand(true);
        if flavour == Flavour::Note {
            crate::highlight::apply(&buffer);
            // The tags' colours are read off the view's own, which it has once it is realised.
            view.connect_realize(|view| {
                if let Some(buffer) = view.buffer().downcast_ref::<sourceview5::Buffer>() {
                    crate::highlight::restyle(buffer, view);
                }
            });
        } else {
            view.add_css_class("notebook-source");
        }

        let count = gtk::Label::builder()
            .label(match (cell.kind(), cell.count()) {
                (Kind::Code, Some(n)) => format!("[{n}]"),
                (Kind::Code, None) => "[ ]".to_string(),
                _ => String::new(),
            })
            .width_chars(6)
            .xalign(1.0)
            .valign(gtk::Align::Start)
            .margin_top(8)
            .build();
        count.add_css_class("notebook-count");

        let outputs = match cell.kind() {
            Kind::Code => Outputs::new(cell.outputs()),
            _ => Outputs::attachments(cell.attachments()),
        };
        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.set_hexpand(true);
        body.append(&view);
        body.append(&outputs.widget);

        let button = |icon: &str, tip: &str| {
            let b = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tip)
                .build();
            b.add_css_class("flat");
            b
        };
        let up = button(
            "go-up-symbolic",
            crate::actions::label_of("win.notebook-move-up"),
        );
        let down = button(
            "go-down-symbolic",
            crate::actions::label_of("win.notebook-move-down"),
        );
        let delete = button(
            "user-trash-symbolic",
            crate::actions::label_of("win.notebook-delete-cell"),
        );
        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("More")
            .build();
        // Its popover made on the first press: one per cell up front would cost a long
        // notebook a popover menu per cell before anything is shown.
        more.set_create_popup_func(|more| {
            if more.menu_model().is_none() {
                more.set_menu_model(Some(&cell_menu()));
            }
        });
        more.add_css_class("flat");
        // Over the editor's top right corner, as VS Code floats a cell's toolbar, so a cell is
        // as tall as its text and its outputs.
        let tools = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tools.add_css_class("notebook-tools");
        tools.set_halign(gtk::Align::End);
        tools.set_valign(gtk::Align::Start);
        for b in [&up, &down, &delete] {
            tools.append(b);
        }
        tools.append(&more);

        let over = gtk::Overlay::builder().child(&body).hexpand(true).build();
        over.add_overlay(&tools);
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        card.add_css_class("notebook-cell");
        card.append(&count);
        card.append(&over);

        let (inserter, add_code, add_markdown) = inserter();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&card);
        root.append(&inserter);
        CellView {
            id: cell.id(),
            kind: cell.kind(),
            root,
            card,
            view,
            buffer,
            outputs,
            tools,
            up,
            down,
            delete,
            more,
            inserter,
            add_code,
            add_markdown,
        }
    }

    pub fn text(&self) -> String {
        let (start, end) = self.buffer.bounds();
        self.buffer.text(&start, &end, true).to_string()
    }

    pub fn set_selected(&self, on: bool) {
        match on {
            true => self.card.add_css_class("selected"),
            false => self.card.remove_css_class("selected"),
        }
    }

    /// Read only, the buttons and the inserter away: a presented notebook.
    pub fn set_presenting(&self, on: bool) {
        self.view.set_editable(!on);
        self.tools.set_visible(!on);
        self.inserter.set_visible(!on);
    }
}

/// The strip that adds a code or a markdown cell where it is, shown while the pointer is over it.
pub fn inserter() -> (gtk::Box, gtk::Button, gtk::Button) {
    let add = |label: &str| {
        let b = gtk::Button::builder().label(label).build();
        b.add_css_class("flat");
        b.add_css_class("caption");
        b
    };
    let (code, markdown) = (add("+ Code"), add("+ Markdown"));
    let strip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    strip.set_halign(gtk::Align::Center);
    strip.add_css_class("notebook-inserter");
    strip.append(&code);
    strip.append(&markdown);
    (strip, code, markdown)
}

/// The cell's More menu: what the palette also does to the selected cell.
fn cell_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = |actions: &[&str]| {
        let s = gio::Menu::new();
        for action in actions {
            s.append(Some(crate::actions::label_of(action)), Some(action));
        }
        menu.append_section(None, &s);
    };
    section(&["win.notebook-insert-above", "win.notebook-insert-below"]);
    section(&[
        "win.notebook-to-code",
        "win.notebook-to-markdown",
        "win.notebook-to-raw",
    ]);
    section(&["win.notebook-split", "win.notebook-join"]);
    menu
}

/// The stylesheet every notebook shares, installed with the first one.
pub fn install_css() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if DONE.with(|d| d.replace(true)) {
        return;
    }
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        ".notebook-cell { border: 1px solid transparent; border-radius: 8px; padding: 4px; } \
         .notebook-cell.selected { border-color: var(--accent-bg-color); } \
         .notebook-tools { opacity: 0; margin: 2px; border-radius: 6px; \
           background-color: var(--view-bg-color); \
           box-shadow: 0 0 0 1px alpha(currentColor, 0.12); } \
         .notebook-tools button { min-height: 24px; min-width: 24px; padding: 0; } \
         .notebook-cell:hover .notebook-tools, .notebook-cell.selected .notebook-tools { opacity: 1; } \
         .notebook-inserter { opacity: 0; } \
         .notebook-inserter button { min-height: 20px; padding: 0 10px; } \
         .notebook-inserter:hover, .notebook-inserter:focus-within { opacity: 1; } \
         .notebook-count { opacity: var(--dim-opacity); font-family: monospace; } \
         textview.notebook-source { border-radius: 6px; \
           box-shadow: inset 0 0 0 1px alpha(currentColor, 0.12); } \
         textview.notebook-output, textview.notebook-output > text { background: none; } \
         textview.notebook-stderr > text { background-color: alpha(var(--error-color), 0.08); }",
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
