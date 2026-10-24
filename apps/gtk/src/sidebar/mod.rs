//! The Files, Search, Info, Git, Ports, Outline, Code and Properties sidebar.
//!
//! Finished widgets and closures in [`Data`] supply vault integration without exposing app
//! state. Each pane receives only the data and operations it uses.

mod breakpoints;
mod code;
mod debug;
mod details;
mod info;
mod outline;
mod ports;
mod python;
mod search;
mod sections;
mod tags;
mod widgets;

pub use breakpoints::Row as BreakpointRow;
#[cfg(feature = "bench")]
pub use debug::Shown as DebugShown;
pub use debug::{Data as DebugData, Watched};
pub use details::{Fact, Group};
pub use outline::{Line, above, outline_note};
pub use ports::Data as PortsData;
pub use python::Data as PythonData;
pub use search::{Answer, Data as SearchData, Query};
pub use tags::Data as TagsData;

use accent_api::Goals;
use accent_api::debug::{Frame, Scope, Thread};
use accent_core::config::{CodePane, InfoPane};
use adw::prelude::*;
use gtk::{glib, pango};
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;
use widgets::path_list;

use crate::widgets::{Debounce, scroller};

/// Debounce visible Tags and Search refreshes after vault changes. Collapses per-file events
/// into one query and gives index writes time to settle, including own saves that emit no event.
/// Matches the delay used for PDF highlights.
const INDEX_SETTLE: Duration = Duration::from_millis(300);

/// Incoming/outgoing arrows for the Git pane. Use an icon name rendered as arrows in both
/// Adwaita and WhiteSur; `network-transmit-receive` appears as a device in WhiteSur (DESIGN.md,
/// Iconography).
const GIT_ICON: &str = "mail-send-receive-symbolic";
/// Where in the file an activated row points, when it points at more than the file itself.
#[derive(Clone)]
pub enum Target {
    /// The byte range a search hit matched.
    Range(Range<usize>),
    /// The tag a row under the Tags section was listed under. The section knows the name and not
    /// where in the note it is written, so the note itself is asked once it is open.
    Tag(String),
}

/// Open a note, over the place in it the activated row names.
type OnOpen = Rc<dyn Fn(&str, Option<Target>)>;

/// Everything the sidebar needs from the index, as closures so it never sees a vault handle.
/// One field per pane that reads one: a pane is built with its own half and cannot reach the
/// rest.
pub struct Data {
    pub search: search::Data,
    pub tags: tags::Data,
    pub ports: ports::Data,
    pub debug: debug::Data,
    pub python: python::Data,
}

pub struct Sidebar {
    root: gtk::Widget,
    switcher: gtk::Widget,
    stack: adw::ViewStack,
    /// Everything that needs an index behind it. `None` in a window with no vault, where the
    /// Info pane holds Details alone.
    panes: Option<VaultPanes>,
    /// Git also belongs to a focused shell in a window without a vault.
    git: Option<(adw::ViewStackPage, gtk::Paned)>,
    /// The Info pane: References, Tags and Details as sections, and what Details shows.
    info: Rc<info::Info>,
    details: details::Details,
    /// Whatever the Outline pane is showing. A `Bin` rather than a list of its own, because what
    /// belongs in it depends entirely on the open tab: a note's headings, a PDF's bookmarks and
    /// thumbnails, or a sentence saying why there is nothing.
    outline_bin: adw::Bin,
    /// The list in `outline_bin` while it holds a document's outline as rows, kept to be refilled
    /// rather than rebuilt while that document is edited or read.
    outline_list: RefCell<Option<outline::List>>,
    /// The Code pane: what holds at the caret beyond the text, and its page, hidden until the
    /// window says the tab in front has something for it.
    code: code::Code,
    code_page: adw::ViewStackPage,
    /// The Properties pane: the diagram in front's own widget, and the page, hidden while no
    /// diagram is in front.
    properties_bin: adw::Bin,
    properties_page: adw::ViewStackPage,
}

/// The panes that read the vault's index.
struct VaultPanes {
    search_entry: gtk::SearchEntry,
    replace_toggle: gtk::ToggleButton,
    all_toggle: gtk::ToggleButton,
    replace_entry: gtk::Entry,
    restart_search: Rc<dyn Fn()>,
    /// The Search pane's half of [`INDEX_SETTLE`], the shape the Tags list's `tags_settle` has.
    search_settle: Debounce,
    /// And its half of `tags_dirty`: the vault moved while another pane was in front.
    search_dirty: Rc<Cell<bool>>,
    /// The Replace All button and what the body is showing, for `ACCENT_BENCH_REPLACE`.
    #[cfg(feature = "bench")]
    apply_replace: gtk::Button,
    #[cfg(feature = "bench")]
    search_state: Rc<dyn Fn() -> (String, Vec<String>, String)>,
    #[cfg(feature = "bench")]
    search_running: Rc<dyn Fn() -> usize>,
    #[cfg(feature = "bench")]
    search_view: gtk::ListView,
    references: gtk::StringList,
    references_stack: gtk::Stack,
    /// The line the References section shows instead of rows. Its words change with what the
    /// tab holds — a note has backlinks, a source file has references — so they are set rather
    /// than built in.
    references_empty: gtk::Label,
    tags_dirty: Rc<Cell<bool>>,
    tags_refill: Rc<dyn Fn()>,
    #[cfg(feature = "bench")]
    tags_names: Rc<dyn Fn() -> Vec<String>>,
    #[cfg(feature = "bench")]
    tags_picked: Rc<dyn Fn() -> Option<String>>,
    #[cfg(feature = "bench")]
    tags_view: gtk::ListView,
    /// Holds a refill of the list on screen back by [`INDEX_SETTLE`], and swallows a burst of
    /// watcher events into one query.
    tags_settle: Debounce,
    /// Refills the tag list if it fell behind while out of sight and is on screen now.
    tags_catch_up: Rc<dyn Fn(&adw::ViewStack)>,
    tags_divider: gtk::Paned,
    /// The Ports pane's page, hidden for the same reason the Git one is: a vault on this machine
    /// has no ssh connection to forward anything over.
    ports_page: adw::ViewStackPage,
    /// Draws the Ports pane's rows again from the connection's list.
    ports_refill: Rc<dyn Fn()>,
    select_tag: Rc<dyn Fn(&str)>,
}

impl Sidebar {
    /// Build Files, Search and Info from `vault`'s tree and index closures. `None` provides only
    /// file Details and Outline for a loose-file window. `on_open` receives the activated
    /// vault-relative path and optional location; `on_reference` receives a row with its line
    /// number.
    pub fn new(
        vault: Option<(gtk::Widget, Data)>,
        git: Option<(gtk::Widget, gtk::Paned)>,
        on_open: impl Fn(&str, Option<Target>) + 'static,
        on_reference: impl Fn(&str) + 'static,
    ) -> Sidebar {
        let on_open: OnOpen = Rc::new(on_open);
        let stack = adw::ViewStack::builder()
            .vexpand(true)
            .enable_transitions(true)
            .transition_duration(crate::widgets::FADE_MS)
            .build();

        // Every pane but Info's Details and the outline is a view of an index, so without one
        // there is nothing for them to show and they are not built at all.
        let details = details::Details::new();
        let mut vault_info = None;
        let mut debug = None;
        let mut python = None;
        let panes = vault.map(|(files, data)| {
            debug = Some(data.debug);
            python = Some(data.python);
            stack.add_titled_with_icon(&files, Some("files"), "Files", "folder-symbolic");

            let search = search::pane(&Rc::new(data.search), &on_open);
            stack.add_titled_with_icon(
                &search.widget,
                Some("search"),
                "Search",
                "system-search-symbolic",
            );

            let tags = tags::pane(&Rc::new(data.tags), &on_open);
            let references = gtk::StringList::new(&[]);
            let (references_stack, references_empty) = references_body(&references, on_reference);
            let info = info::Info::new(
                Some((references_stack.upcast_ref(), &tags.widget)),
                &details.root,
            );
            stack.add_titled_with_icon(&info.root, Some("info"), "Info", info::ICON);
            vault_info = Some(info.clone());

            let (ports, ports_refill) = ports::pane(&Rc::new(data.ports));
            stack.add_titled_with_icon(&ports, Some("ports"), "Ports", ports::ICON);
            let ports_page = stack.page(&ports);
            // Hidden until the window says its vault is on another machine.
            ports_page.set_visible(false);

            // Lazy fill: a background reindex only flips the flag of whichever of the two lists
            // is not being looked at, and it costs that list one query the first time it is shown
            // again. The Search pane's is its query re-run, which is 0.8 ms ranked and some 60 ms
            // as an exact scan on the generated 40k-file vault, and nothing at all while the box
            // is empty or the vault has not moved. The tag list is on screen once the Info pane
            // is in front with the Tags section open, so opening the section catches up too.
            let tags_catch_up: Rc<dyn Fn(&adw::ViewStack)> = Rc::new({
                let (info, dirty, refill) = (
                    Rc::downgrade(&info),
                    tags.dirty.clone(),
                    tags.refill.clone(),
                );
                move |stack| {
                    if info
                        .upgrade()
                        .is_some_and(|info| live(stack, &info, "tags"))
                        && dirty.replace(false)
                    {
                        refill();
                    }
                }
            });
            stack.connect_visible_child_notify({
                let (searched, restart) = (search.dirty.clone(), search.restart.clone());
                let catch_up = tags_catch_up.clone();
                move |stack| match stack.visible_child_name().as_deref() {
                    Some("info") => catch_up(stack),
                    Some("search") if searched.replace(false) => restart(),
                    _ => {}
                }
            });
            info.tags.root.connect_expanded_notify(glib::clone!(
                #[weak]
                stack,
                #[strong]
                tags_catch_up,
                move |_| tags_catch_up(&stack)
            ));
            (
                search,
                tags,
                (references, references_stack, references_empty),
                tags_catch_up,
                (ports_page, ports_refill),
            )
        });

        let info = vault_info.unwrap_or_else(|| {
            let info = info::Info::new(None, &details.root);
            stack.add_titled_with_icon(&info.root, Some("info"), "Info", info::ICON);
            info
        });

        let git = git.map(|(widget, divider)| {
            stack.add_titled_with_icon(&widget, Some("git"), "Git", GIT_ICON);
            let page = stack.page(&widget);
            page.set_visible(false);
            (page, divider)
        });

        let outline_bin = adw::Bin::builder().vexpand(true).build();
        outline_bin.set_child(Some(&outline::empty()));
        stack.add_titled_with_icon(&outline_bin, Some("outline"), "Outline", outline::ICON);

        let code = code::Code::new(debug, python);
        stack.add_titled_with_icon(&code.root, Some("code"), "Code", code::ICON);
        let code_page = stack.page(&code.root);
        code_page.set_visible(false);

        let properties_bin = adw::Bin::builder().vexpand(true).build();
        stack.add_titled_with_icon(
            &properties_bin,
            Some("properties"),
            "Properties",
            "document-properties-symbolic",
        );
        let properties_page = stack.page(&properties_bin);
        // Hidden until a diagram is in front: nothing else has properties to show.
        properties_page.set_visible(false);

        // Icons, because four labels do not fit a 200 px sidebar without truncating. The switcher
        // gives every toggle the page title as its tooltip, so icon-only stays discoverable. No
        // vertical alignment of its own: as a header title widget it takes the header's full
        // content height, which is what puts its toggles on the same line as the buttons opposite.
        // Centred rather than filling: a toggle that stretches to the header's height comes out
        // taller than it is wide, and these are square icon buttons.
        let switcher = adw::InlineViewSwitcher::builder()
            .stack(&stack)
            .display_mode(adw::InlineViewSwitcherDisplayMode::Icons)
            .margin_start(6)
            .margin_end(6)
            .valign(gtk::Align::Center)
            .build();
        switcher.add_css_class("flat");

        Sidebar {
            root: stack.clone().upcast(),
            switcher: switcher.upcast(),
            stack,
            git,
            panes: panes.map(
                |(search, tags, references, tags_catch_up, (ports_page, ports_refill))| {
                    let (references, references_stack, references_empty) = references;
                    VaultPanes {
                        search_entry: search.entry,
                        replace_toggle: search.replace_toggle,
                        all_toggle: search.all_toggle,
                        replace_entry: search.replace_entry,
                        restart_search: search.restart,
                        search_settle: Debounce::new(INDEX_SETTLE),
                        search_dirty: search.dirty,
                        #[cfg(feature = "bench")]
                        apply_replace: search.apply,
                        #[cfg(feature = "bench")]
                        search_state: search.state,
                        #[cfg(feature = "bench")]
                        search_running: search.running,
                        #[cfg(feature = "bench")]
                        search_view: search.view,
                        references,
                        references_stack,
                        references_empty,
                        tags_dirty: tags.dirty,
                        tags_refill: tags.refill,
                        #[cfg(feature = "bench")]
                        tags_names: tags.names,
                        #[cfg(feature = "bench")]
                        tags_picked: tags.picked,
                        #[cfg(feature = "bench")]
                        tags_view: tags.view,
                        tags_settle: Debounce::new(INDEX_SETTLE),
                        tags_catch_up,
                        tags_divider: tags.divider,
                        ports_page,
                        ports_refill,
                        select_tag: tags.select,
                    }
                },
            ),
            info,
            details,
            outline_bin,
            outline_list: RefCell::new(None),
            code,
            code_page,
            properties_bin,
            properties_page,
        }
    }

    pub fn add_bibliography(&self, widget: &impl IsA<gtk::Widget>) {
        self.stack
            .add_titled_with_icon(
                widget,
                Some("bibliography"),
                "Bibliography",
                "accessories-dictionary-symbolic",
            )
            .set_visible(false);
    }

    pub fn set_bibliography_visible(&self, visible: bool) {
        if let Some(child) = self.stack.child_by_name("bibliography") {
            if !visible && self.is_showing("bibliography") {
                self.stack.set_visible_child_name("files");
            }
            self.stack.page(&child).set_visible(visible);
        }
    }

    /// Show a diagram's properties in their pane, or with `None` take the pane away — to the
    /// Outline first when it is the one on screen, so the stack is never left showing nothing.
    pub fn set_properties(&self, content: Option<&gtk::Widget>) {
        match content {
            Some(widget) => {
                if self.properties_bin.child().as_ref() != Some(widget) {
                    self.properties_bin.set_child(Some(widget));
                }
                self.properties_page.set_visible(true);
            }
            None => {
                if self.is_showing("properties") {
                    self.stack.set_visible_child_name("outline");
                }
                self.properties_page.set_visible(false);
                self.properties_bin.set_child(gtk::Widget::NONE);
            }
        }
    }

    /// Show or hide the Code pane, which the window decides: a hidden one on screen gives way to
    /// the Outline, as the Properties pane does.
    pub fn set_code_visible(&self, on: bool) {
        if !on && self.is_showing("code") {
            self.stack.set_visible_child_name("outline");
        }
        self.code_page.set_visible(on);
    }

    /// Clear the Code pane if what it shows is about another tab than `key`, the one in front,
    /// and say the file is being checked (`checking`) while no answer has come for it.
    pub fn sync_code(&self, key: &str, checking: bool) {
        self.code.sync(key, checking);
    }

    /// Show the goals at the caret of the tab `key` in the Code pane, in `font`, the editor's at
    /// the tab's zoom; `None` says there are none.
    pub fn set_goals(
        &self,
        key: &str,
        goals: Option<&Goals>,
        font: Option<&pango::FontDescription>,
    ) {
        self.code.set(key, goals, font);
    }

    /// A debug session's state in the Code pane: the strip saying `line`, `why` in its tooltip,
    /// and whether the program is `paused`. Running, Call Stack and Variables are cleared.
    pub fn set_debug_state(&self, line: &str, why: Option<&str>, paused: bool) {
        if let Some(debug) = &self.code.debug {
            debug.set_state(line, why, paused);
            self.code.set_debugging(true);
        }
    }

    /// The program's threads, and the one it paused on open on its calls.
    pub fn set_debug_threads(&self, threads: &[Thread], paused: Option<(i64, Vec<Frame>)>) {
        if let Some(debug) = &self.code.debug {
            debug.set_threads(threads, paused);
        }
    }

    pub fn select_debug_frame(&self, frame: i64) {
        if let Some(debug) = &self.code.debug {
            debug.select_frame(frame);
        }
    }

    /// The selected frame's scopes, as the roots of Variables.
    pub fn set_debug_scopes(&self, scopes: &[Scope]) {
        if let Some(debug) = &self.code.debug {
            debug.set_scopes(scopes);
        }
    }

    /// A Debug Console answer that holds more, atop Variables until the program moves on.
    pub fn set_debug_answer(&self, answer: accent_api::debug::Variable) {
        if let Some(debug) = &self.code.debug {
            debug.set_answer(answer);
        }
    }

    /// The Watch section's rows: each expression, with its value while the program is paused.
    pub fn set_debug_watches(&self, rows: &[Watched]) {
        if let Some(debug) = &self.code.debug {
            debug.set_watches(rows);
        }
    }

    /// The switches atop Breakpoints: the exceptions the program can stop at.
    pub fn set_debug_exceptions(&self, filters: &[accent_api::debug::ExceptionFilter]) {
        if let Some(debug) = &self.code.debug {
            debug.set_exceptions(filters);
        }
    }

    /// The Breakpoints section's rows, each a file key and a breakpoint, and what the adapter
    /// takes beside a line.
    pub fn set_breakpoint_rows(
        &self,
        rows: &[BreakpointRow],
        supports: accent_api::debug::Supports,
    ) {
        if let Some(debug) = &self.code.debug {
            debug.set_breakpoints(rows, supports);
        }
    }

    /// The Breakpoints rows as they read, `path:line` and what is set beside it.
    #[cfg(feature = "bench")]
    pub fn breakpoint_rows(&self) -> Vec<String> {
        let debug = self.code.debug.as_ref();
        debug.map_or_else(Vec::new, |d| d.breakpoint_rows().shown())
    }

    /// Edit a breakpoint through its row as a reader does: its button, `values` typed into the
    /// fields with those titles, and Enter.
    #[cfg(feature = "bench")]
    pub fn edit_breakpoint_row(&self, key: &str, line: u32, values: &[(&str, &str)]) {
        if let Some(debug) = &self.code.debug {
            debug.breakpoint_rows().edit_as_typed(key, line, values);
        }
    }

    /// The notebook in front's kernel in the Code pane: its strip saying `line`, `why` in its
    /// tooltip, with a spinner while it is `busy` running cells, or put away (`None`).
    pub fn set_kernel(&self, shown: Option<(&str, Option<&str>)>, busy: bool) {
        self.code.set_kernel(shown, busy);
    }

    #[cfg(feature = "bench")]
    pub fn kernel_line(&self) -> Option<String> {
        self.code.kernel_line()
    }

    /// Name `command`, the vault's Python, on the Code pane's Python Environment dropdowns; empty
    /// is the default configuration.
    pub fn set_python(&self, command: &[String]) {
        self.code.set_python(command);
    }

    /// The Python Environment dropdown of the Code pane's strip that is up, if one is.
    pub fn python_button(&self) -> Option<gtk::MenuButton> {
        self.code.python_button()
    }

    /// No session: Call Stack and Variables go, and the strip, offering Start Debugging, and
    /// Breakpoints stay only while the tab in front can be debugged (`debuggable`).
    pub fn clear_debug(&self, debuggable: bool) {
        if let Some(debug) = &self.code.debug {
            debug.set_idle(debuggable);
            self.code.set_debugging(debuggable);
        }
    }

    /// The goals the Code pane shows, for `ACCENT_BENCH_GOALS`.
    #[cfg(feature = "bench")]
    pub fn shown_goals(&self) -> Option<Vec<String>> {
        self.code.goals()
    }

    /// Open the Debug Console's answer atop Variables, for `ACCENT_BENCH_DEBUG`.
    #[cfg(feature = "bench")]
    pub fn open_debug_answer(&self) {
        if let Some(debug) = &self.code.debug {
            debug.open_answer();
        }
    }

    /// The strip's line, the calls Call Stack lists, the values Variables has open and Watch's
    /// rows, for `ACCENT_BENCH_DEBUG`.
    #[cfg(feature = "bench")]
    pub fn debug_shown(&self) -> Option<DebugShown> {
        self.code.debug.as_ref().map(|debug| debug.shown())
    }

    /// Put `divider` back to its default if it is one of the sidebar's own, reporting whether it
    /// was. The window's double-click handler asks every divider owner in turn, so the rule for a
    /// pane lives next to the pane rather than in the shell.
    pub fn reset_divider(&self, divider: &gtk::Paned) -> bool {
        if self.info.reset_divider(divider) || self.code.reset_divider(divider) {
            return true;
        }
        if self.git.as_ref().is_some_and(|(_, git)| git == divider) {
            divider
                .set_position(divider.height() * crate::git::GIT_SHARE.0 / crate::git::GIT_SHARE.1);
            return true;
        }
        let Some(panes) = self.panes.as_ref() else {
            return false;
        };
        let share = if divider == &panes.tags_divider {
            tags::SHARE
        } else {
            return false;
        };
        divider.set_position(divider.height() * share.0 / share.1);
        true
    }

    /// The pane switcher, the title widget of the sidebar header so that it shares the header band
    /// with the main header instead of taking a band of its own. `AdwHeaderBar` centres a title
    /// widget, so this hands out the switcher itself with no wrapper to do the centring.
    pub fn switcher(&self) -> &gtk::Widget {
        &self.switcher
    }

    /// The panes themselves, for the sidebar column's content.
    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    /// Replace what the References section lists, and say what its emptiness would mean: a
    /// note's backlinks and a source file's references are the same section asking different
    /// questions. `title` names what the rows are — the language server's References, the
    /// Backlinks, a definition's several answers — and `None` keeps the one shown. `rows` is
    /// `None` while the answer is still coming, which empties the list without calling it empty.
    pub fn set_references(&self, title: Option<&str>, rows: Option<&[String]>, empty: &str) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        let section = &self.info.references;
        if let Some(title) = title {
            section.set_title(title);
        }
        section.set_count(rows.map(<[String]>::len));
        let list: Vec<&str> = rows
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .collect();
        panes
            .references
            .splice(0, panes.references.n_items(), list.as_slice());
        panes.references_empty.set_text(empty);
        panes.references_stack.set_visible_child_name(match rows {
            Some([]) => "empty",
            _ => "list",
        });
    }

    /// Fit the Info pane to the tab `key` in front (empty for none): its sections for a file,
    /// with Tags over a markdown note, Details alone over a shell, and a status page where there
    /// is neither.
    pub fn sync_info(&self, key: &str, file: bool, shell: bool, note: bool) {
        self.info.sync(key, file, shell, note);
        if let Some(panes) = self.panes.as_ref() {
            (panes.tags_catch_up)(&self.stack);
        }
    }

    /// Bring the Info pane to the front with the section `name` — "references", "tags" or
    /// "details" — open, the keyboard left where it is.
    pub fn show_section(&self, name: &str) {
        self.stack.set_visible_child_name("info");
        self.info.open(name);
        // Opening a section that was open but hidden notifies nothing.
        if let Some(panes) = self.panes.as_ref() {
            (panes.tags_catch_up)(&self.stack);
        }
    }

    /// Whether the section `name` is on screen: the Info pane in front with it shown and open.
    pub fn section_live(&self, name: &str) -> bool {
        live(&self.stack, &self.info, name)
    }

    /// Call `f` with a section's name and whether it is open now, whenever one is folded or
    /// unfolded.
    pub fn connect_section_toggled(&self, f: impl Fn(&str, bool) + 'static) {
        let f = Rc::new(f);
        for name in [
            "references",
            "tags",
            "details",
            "goals",
            "expected",
            "stack",
            "variables",
            "breakpoints",
        ] {
            if let Some(section) = self.info.section(name).or_else(|| self.code.section(name)) {
                let f = f.clone();
                section
                    .root
                    .connect_expanded_notify(move |e| f(name, e.is_expanded()));
            }
        }
    }

    /// Show the file's facts in the Details section.
    pub fn set_details(&self, groups: &[Group]) {
        self.details.set(groups);
    }

    /// The Info pane's sections, for the session.
    pub fn info_saved(&self) -> InfoPane {
        self.info.saved()
    }

    pub fn restore_info(&self, saved: &InfoPane) {
        self.info.restore(saved);
    }

    /// The Code pane's sections, for the session.
    pub fn code_saved(&self) -> CodePane {
        self.code.saved()
    }

    pub fn restore_code(&self, saved: &CodePane) {
        self.code.restore(saved);
    }

    /// Which pane is on screen, for the callers that only refresh what is being looked at.
    pub fn is_showing(&self, name: &str) -> bool {
        self.stack.visible_child_name().as_deref() == Some(name)
    }

    /// Whether this sidebar has the named pane at all, and is showing it. A window with no vault
    /// has Info, Outline and Git when its focused shell has a repository; chords for other panes
    /// must not open a column that cannot answer them.
    pub fn has_pane(&self, name: &str) -> bool {
        self.stack
            .child_by_name(name)
            .is_some_and(|child| self.stack.page(&child).is_visible())
    }

    /// Show or hide the Git pane. It starts hidden and the pane's first refresh decides.
    pub fn set_git_visible(&self, on: bool) {
        if let Some((page, _)) = self.git.as_ref() {
            page.set_visible(on);
        }
    }

    /// Show or hide the Ports pane. It starts hidden; the window turns it on once its vault
    /// turns out to be on another machine.
    pub fn set_ports_visible(&self, on: bool) {
        if let Some(panes) = self.panes.as_ref() {
            panes.ports_page.set_visible(on);
        }
    }

    /// Draw the Ports pane again from the connection's forwards: a reconnect drops one that would
    /// not come back.
    pub fn refresh_ports(&self) {
        if let Some(panes) = self.panes.as_ref() {
            (panes.ports_refill)();
        }
    }

    /// Replace what the Outline pane shows; `None` puts the empty state back.
    pub fn set_outline(&self, content: Option<&gtk::Widget>) {
        self.outline_list.take();
        match content {
            Some(widget) => self.outline_bin.set_child(Some(widget)),
            None => self.outline_bin.set_child(Some(&outline::empty())),
        }
    }

    /// Whether the pane shows `below` under a sentence ([`above`]), as it shows a PDF's
    /// thumbnail strip under "No Bookmarks": a refill leaves that be, since taking the strip out
    /// from under the pointer ends its hover.
    pub fn shows_below(&self, below: &gtk::Widget) -> bool {
        self.outline_list.borrow().is_none()
            && below
                .parent()
                .is_some_and(|split| self.outline_bin.child() == Some(split))
    }

    /// Show the outline of the document `key` as rows that jump, over `below` when there is one
    /// (a PDF's thumbnail strip). The list on screen is refilled when it is already that
    /// document's, so an edit, a save, a language server's answer or a page edit leaves it
    /// scrolled where it was; another document gets a new list, from the top, and `true` back.
    pub fn set_outline_rows<T: Copy + 'static>(
        &self,
        key: &str,
        rows: &[(Line, T)],
        on_jump: impl Fn(T) + 'static,
        below: Option<&gtk::Widget>,
    ) -> bool {
        let mut kept = self.outline_list.borrow_mut();
        let fresh = kept
            .as_ref()
            .is_none_or(|list| list.key != key || list.below.as_ref() != below);
        if fresh {
            let list = outline::List::new(key, below.cloned());
            self.outline_bin.set_child(Some(&list.root));
            *kept = Some(list);
        }
        if let Some(list) = kept.as_ref() {
            list.fill(rows, on_jump);
        }
        fresh
    }

    /// Select `row` of the document `key`'s outline, the one its caret or its reader is in, and
    /// scroll `shown` into view; `None` selects nothing, or scrolls nowhere.
    pub fn follow_outline(&self, key: &str, row: Option<usize>, shown: Option<usize>) {
        if let Some(list) = self.outline_list.borrow().as_ref()
            && list.key == key
        {
            list.follow(row, shown);
        }
    }

    /// Call `f` whenever another pane comes to the front.
    pub fn connect_pane_shown(&self, f: impl Fn() + 'static) {
        self.stack.connect_visible_child_notify(move |_| f());
    }

    /// What the Outline pane is showing, which is what `ACCENT_BENCH_TABS` reads to say whether a
    /// closed document left its outline behind.
    #[cfg(feature = "bench")]
    pub fn outline_child(&self) -> Option<gtk::Widget> {
        self.outline_bin.child()
    }

    /// The Outline pane's rows as they read, a lead before its text and a heading in capitals,
    /// and `activate` that of them, as a click does. What `ACCENT_BENCH_PDF=comments:` reads.
    #[cfg(feature = "bench")]
    pub fn outline_lines(&self, activate: Option<usize>) -> Vec<String> {
        let Some((lines, view)) = self.outline_list.borrow().as_ref().map(|l| l.lines()) else {
            return Vec::new();
        };
        // Out of the borrow: what the row does may refill the list.
        if let Some(row) = activate {
            view.emit_by_name::<()>("activate", &[&(row as u32)]);
        }
        lines
    }

    /// Mark Tags stale. Hidden lists defer querying until shown; visible lists refresh after
    /// [`INDEX_SETTLE`] to coalesce events and wait for indexing.
    pub fn mark_tags_dirty(&self) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        if !live(&self.stack, &self.info, "tags") {
            panes.tags_dirty.set(true);
            return;
        }
        panes.tags_dirty.set(false);
        let refill = panes.tags_refill.clone();
        panes.tags_settle.call(move || refill());
    }

    /// The tag names the Tags section is showing, and the one selected: what `ACCENT_BENCH_TAGS`
    /// reads, a refill being invisible from anywhere else.
    #[cfg(feature = "bench")]
    pub fn tag_names(&self) -> Vec<String> {
        match self.panes.as_ref() {
            Some(panes) => (panes.tags_names)(),
            None => Vec::new(),
        }
    }

    #[cfg(feature = "bench")]
    pub fn selected_tag(&self) -> Option<String> {
        self.panes.as_ref().and_then(|panes| (panes.tags_picked)())
    }

    /// What the Info pane shows, and the Details rows, as `ACCENT_BENCH_INFO` prints them.
    #[cfg(feature = "bench")]
    pub fn info_state(&self) -> (String, Vec<String>) {
        (self.info.state(), self.details.lines())
    }

    /// Fold or unfold the section `name`, as a click on its header does.
    #[cfg(feature = "bench")]
    pub fn fold_section(&self, name: &str, open: bool) {
        if let Some(section) = self.info.section(name) {
            section.root.set_expanded(open);
        }
    }

    /// The tag list itself, for `ACCENT_BENCH_SCROLL`.
    #[cfg(feature = "bench")]
    pub fn tags_view(&self) -> Option<gtk::ListView> {
        self.panes.as_ref().map(|panes| panes.tags_view.clone())
    }

    /// Show a pane by name: "files", "search", "info", "git", "ports", "outline", "code" or
    /// "properties",
    /// focusing its entry where there is one, all it holds selected, as the find bar's Ctrl+F
    /// leaves its box: what is typed next is a new query.
    pub fn show_pane(&self, name: &str) {
        // A pane this sidebar does not have leaves it where it was, which for a window with no
        // vault means Info or the outline stays up whatever chord was pressed.
        if !self.has_pane(name) {
            return;
        }
        self.stack.set_visible_child_name(name);
        if let (Some(panes), "search") = (self.panes.as_ref(), name) {
            // `GtkSearchEntry` selects nothing when it takes the keyboard, where `GtkEntry` does.
            panes.search_entry.grab_focus();
            panes.search_entry.select_region(0, -1);
        }
    }

    /// Ctrl+Shift+H: the Search pane with its replace row open, focused where there is still
    /// something to type.
    pub fn show_replace(&self) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        self.show_pane("search");
        panes.replace_toggle.set_active(true);
        if !panes.search_entry.text().is_empty() {
            panes.replace_entry.grab_focus();
            panes.replace_entry.select_region(0, -1);
        }
    }

    /// Put `text` in the Search pane's box, which runs it: Ctrl+Shift+F or Ctrl+Shift+H over a
    /// selection searches for what is selected. Nothing selected never reaches here, so the box
    /// then keeps whatever it already holds — VS Code's behaviour.
    pub fn set_search_text(&self, text: &str) {
        if let Some(panes) = self.panes.as_ref() {
            panes.search_entry.set_text(text);
        }
    }

    /// Flip the Search pane's All toggle, showing the pane first: the same thing clicking the
    /// button does, for the palette and for anyone who binds a chord to it.
    pub fn toggle_search_all(&self) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        self.show_pane("search");
        panes.all_toggle.set_active(!panes.all_toggle.is_active());
    }

    /// Put `text` in the Search pane's replace box, which re-runs the query with the preview.
    /// The counterpart of [`set_search_text`](Self::set_search_text), for `ACCENT_BENCH_REPLACE`.
    #[cfg(feature = "bench")]
    pub fn set_replace_text(&self, text: &str) {
        if let Some(panes) = self.panes.as_ref() {
            panes.replace_entry.set_text(text);
        }
    }

    /// Press Replace All, as a click on the button does. Headless, that is the only way in: the
    /// button is in the sidebar and Xvfb has nothing to click it with.
    #[cfg(feature = "bench")]
    pub fn press_replace_all(&self) {
        if let Some(panes) = self.panes.as_ref() {
            panes.apply_replace.emit_clicked();
        }
    }

    /// Which page the Search pane's body is showing — "prompt", "results", "empty" or
    /// "invalid" — the rows on it by name, and its result count. What `ACCENT_BENCH_REPLACE`
    /// and `ACCENT_BENCH_SEARCH` read.
    #[cfg(feature = "bench")]
    pub fn search_state(&self) -> (String, Vec<String>, String) {
        match self.panes.as_ref() {
            Some(panes) => (panes.search_state)(),
            None => (String::new(), Vec::new(), String::new()),
        }
    }

    /// How many of the Search pane's queries are on worker threads, for `ACCENT_BENCH_SEARCH`.
    #[cfg(feature = "bench")]
    pub fn search_running(&self) -> usize {
        self.panes
            .as_ref()
            .map_or(0, |panes| (panes.search_running)())
    }

    /// The Search pane's query and replace boxes, for `ACCENT_BENCH_SEARCH=seed:`.
    #[cfg(feature = "bench")]
    pub fn search_boxes(&self) -> Option<[gtk::Editable; 2]> {
        let panes = self.panes.as_ref()?;
        Some([
            panes.search_entry.clone().upcast(),
            panes.replace_entry.clone().upcast(),
        ])
    }

    /// The Search pane's list of rows, for `ACCENT_BENCH_SEARCH=more:`.
    #[cfg(feature = "bench")]
    pub fn search_view(&self) -> Option<gtk::ListView> {
        self.panes.as_ref().map(|panes| panes.search_view.clone())
    }

    /// What the Replace All button says and whether it can be pressed. What
    /// `ACCENT_BENCH_SEARCH` reads beside the rows.
    #[cfg(feature = "bench")]
    pub fn replace_all_state(&self) -> (String, bool) {
        match self.panes.as_ref() {
            Some(panes) => (
                panes.apply_replace.label().unwrap_or_default().to_string(),
                panes.apply_replace.is_sensitive(),
            ),
            None => (String::new(), false),
        }
    }

    /// Refresh active search after vault changes or own saves, debounced through
    /// [`INDEX_SETTLE`]. Empty queries do nothing; hidden panes keep a dirty flag and query on
    /// next show. This avoids per-event work while preventing stale results.
    pub fn requery_search_soon(&self) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        if panes.search_entry.text().trim().is_empty() {
            return;
        }
        if !self.is_showing("search") {
            return panes.search_dirty.set(true);
        }
        panes.search_dirty.set(false);
        let restart = panes.restart_search.clone();
        panes.search_settle.call(move || restart());
    }

    /// Ask the search question again now, if one is on screen, and leave the flag behind if one is
    /// not. What the window calls when the answer would have changed without the box being touched
    /// and without the vault moving — a git refresh moving the ignore set, which is one call, not
    /// a batch, and which reaches rows nobody is looking at the same way a write does.
    pub fn requery_search(&self) {
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        if !self.is_showing("search") {
            if !panes.search_entry.text().trim().is_empty() {
                panes.search_dirty.set(true);
            }
            return;
        }
        panes.search_dirty.set(false);
        (panes.restart_search)();
    }

    /// Show the Tags section with `tag` already selected.
    pub fn show_tag(&self, tag: &str) {
        // Ordering matters: this refills the tag list if it is dirty, and the refill clears the
        // selection, so the tag has to be picked afterwards.
        let Some(panes) = self.panes.as_ref() else {
            return;
        };
        self.show_section("tags");
        (panes.select_tag)(tag);
    }
}

/// Whether the Info pane's section `name` is on screen: the pane in front, showing its sections,
/// with that one shown and open.
fn live(stack: &adw::ViewStack, info: &info::Info, name: &str) -> bool {
    stack.visible_child_name().as_deref() == Some("info")
        && info.root.visible_child_name().as_deref() == Some("sections")
        && info.section(name).is_some_and(widgets::Section::is_open)
}

/// The References section's body: the rows, and the line shown instead when there are none.
fn references_body(
    model: &gtk::StringList,
    on_reference: impl Fn(&str) + 'static,
) -> (gtk::Stack, gtk::Label) {
    let stack = gtk::Stack::builder().vexpand(true).build();
    // The window sets the words to suit what the tab holds (`references::references_empty`).
    let empty = gtk::Label::builder()
        .xalign(0.0)
        .valign(gtk::Align::Start)
        .wrap(true)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    empty.add_css_class("dim-label");
    stack.add_named(&empty, Some("empty"));
    let list = path_list(model, crate::references::reference_icon, on_reference);
    stack.add_named(&scroller(&list), Some("list"));
    (stack, empty)
}
