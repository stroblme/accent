//! accent desktop app: GTK4 + libadwaita shell (spike B).
//!
//! `accent <vault-dir>`: opens the vault's index (creating it if missing), shows the tree from the
//! DB immediately, then reconciles on a background thread so the window is never blocked.

mod editor;
mod highlight;
mod switcher;
mod tree;

use accent_core::fs::{self, SaveError};
use accent_core::index::{Index, Progress, ReconcileStats, default_db_path};
use adw::prelude::*;
use editor::Tab;
use gtk::{gdk, gio, glib, pango};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const APP_ID: &str = "io.github.stroblme.Accent";

/// What the switcher lists before the user types anything.
const RECENT_NOTES: usize = 50;

/// Startup milestones: `RUST_LOG=accent=debug accent <vault>` prints ms since process start at
/// "main", "tree populated", "window mapped"/"presented" and "reconcile done". Keeping these makes
/// a regression in time-to-window visible without reaching for a profiler.
static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn ms() -> u128 {
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis()
}

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tracing::debug!(t_ms = ms(), "main");

    let Some(vault) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: accent <vault-dir> [note.md]");
        return glib::ExitCode::FAILURE;
    };
    let vault = match vault.canonicalize() {
        Ok(v) if v.is_dir() => v,
        _ => {
            eprintln!("not a directory: {}", vault.display());
            return glib::ExitCode::FAILURE;
        }
    };
    let note = std::env::args().nth(2);

    let app = adw::Application::builder()
        .application_id(APP_ID)
        // The vault comes from argv, which GApplication would otherwise try to parse itself.
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_command_line(|app, _| {
        app.activate();
        glib::ExitCode::SUCCESS
    });
    app.connect_activate(move |app| build_window(app, vault.clone(), note.clone()));
    app.run()
}

// --------------------------------------------------------------------------------- app state

struct App {
    vault: PathBuf,
    index: Rc<RefCell<Index>>,
    window: adw::ApplicationWindow,
    tabs: adw::TabView,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    status: gtk::Label,
    root: gio::ListStore,
    backlinks: gtk::StringList,
    open: RefCell<HashMap<String, Rc<Tab>>>,
    /// Set once, by `sidebar`, which cannot run before `App` exists.
    tree: OnceCell<tree::Tree>,
}

impl App {
    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn active(&self) -> Option<Rc<Tab>> {
        let page = self.tabs.selected_page()?;
        self.open
            .borrow()
            .values()
            .find(|t| t.page == page)
            .cloned()
    }

    fn open_note(self: &Rc<Self>, rel: &str) {
        if let Some(tab) = self.open.borrow().get(rel) {
            self.tabs.set_selected_page(&tab.page);
            return;
        }
        match editor::open(&self.vault, rel, &self.tabs) {
            Ok(tab) => {
                self.tabs.set_selected_page(&tab.page);
                self.open.borrow_mut().insert(rel.to_string(), tab);
                self.sync_active();
            }
            Err(e) => self.toast(&format!("Cannot open {rel}: {e}")),
        }
    }

    /// Keep the window subtitle and the backlinks pane in step with the selected tab.
    fn sync_active(&self) {
        let Some(tab) = self.active() else {
            self.title.set_subtitle("");
            self.backlinks.splice(0, self.backlinks.n_items(), &[]);
            return;
        };
        self.title.set_subtitle(&tab.rel);
        let links = self.index.borrow().backlinks(&tab.rel).unwrap_or_default();
        let mut seen: Vec<&str> = Vec::new();
        for l in &links {
            if !seen.contains(&l.src_rel_path.as_str()) {
                seen.push(&l.src_rel_path);
            }
        }
        self.backlinks.splice(0, self.backlinks.n_items(), &seen);
    }

    fn save_active(self: &Rc<Self>) {
        let Some(tab) = self.active() else { return };
        let text = tab.text();
        match fs::write_note(&tab.path, &text, tab.etag.get()) {
            Ok(etag) => {
                tab.mark_clean(etag);
                self.toast("Saved");
            }
            Err(SaveError::ChangedOnDisk { .. }) => self.ask_overwrite(&tab, text),
            Err(e) => self.toast(&format!("Save failed: {e}")),
        }
    }

    fn ask_overwrite(self: &Rc<Self>, tab: &Rc<Tab>, text: String) {
        let dialog = adw::AlertDialog::new(
            Some("File changed on disk"),
            Some(&format!(
                "{} was modified elsewhere since you opened it.",
                tab.rel
            )),
        );
        dialog.add_responses(&[
            ("cancel", "Cancel"),
            ("reload", "Reload"),
            ("overwrite", "Overwrite"),
        ]);
        dialog.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let app = self.clone();
        let tab = tab.clone();
        dialog.choose(
            Some(&self.window),
            gio::Cancellable::NONE,
            move |response| match response.as_str() {
                "reload" => {
                    if let Err(e) = tab.reload() {
                        app.toast(&format!("Reload failed: {e}"));
                    }
                }
                "overwrite" => match fs::write_note(&tab.path, &text, None) {
                    Ok(etag) => {
                        tab.mark_clean(etag);
                        app.toast("Overwritten");
                    }
                    Err(e) => app.toast(&format!("Save failed: {e}")),
                },
                _ => {}
            },
        );
    }
}

// ------------------------------------------------------------------------------ construction

fn build_window(gtk_app: &adw::Application, vault: PathBuf, note: Option<String>) {
    if let Some(win) = gtk_app.active_window() {
        win.present();
        return;
    }
    install_document_font();

    let db = default_db_path(&vault);
    let index = match Index::open(&db) {
        Ok(i) => Rc::new(RefCell::new(i)),
        Err(e) => {
            eprintln!("cannot open index {}: {e}", db.display());
            return;
        }
    };

    let vault_name = vault
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| vault.display().to_string());
    let title = adw::WindowTitle::new(&vault_name, "");
    let tabs = adw::TabView::new();
    let toasts = adw::ToastOverlay::new();
    let status = gtk::Label::builder().visible(false).build();
    status.add_css_class("dim-label");
    let root = gio::ListStore::new::<gtk::StringObject>();
    let backlinks = gtk::StringList::new(&[]);

    let window = adw::ApplicationWindow::builder()
        .application(gtk_app)
        .default_width(1100)
        .default_height(760)
        .build();

    let app = Rc::new(App {
        vault,
        index: index.clone(),
        window: window.clone(),
        tabs: tabs.clone(),
        title: title.clone(),
        toasts: toasts.clone(),
        status: status.clone(),
        root: root.clone(),
        backlinks: backlinks.clone(),
        open: RefCell::new(HashMap::new()),
        tree: OnceCell::new(),
    });

    // Populate straight from the DB: the window must be up before reconcile finishes.
    tree::fill(&root, &index, "");
    tracing::debug!(t_ms = ms(), rows = root.n_items(), "tree populated");

    let split = adw::OverlaySplitView::builder()
        .sidebar(&sidebar(&app))
        .content(&content(&tabs))
        .min_sidebar_width(200.0)
        .max_sidebar_width(420.0)
        .sidebar_width_fraction(0.24)
        .build();
    toasts.set_child(Some(&split));

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    let toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Toggle sidebar")
        .build();
    toggle
        .bind_property("active", &split, "show-sidebar")
        .bidirectional()
        .sync_create()
        .build();
    toggle.set_active(true);
    header.pack_start(&toggle);
    header.pack_end(&menu_button());
    header.pack_end(&status);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    window.set_content(Some(&toolbar));

    install_actions(gtk_app, &app);

    tabs.connect_close_page({
        let app = app.clone();
        move |_, page| {
            app.open.borrow_mut().retain(|_, t| t.page != *page);
            glib::Propagation::Proceed
        }
    });
    tabs.connect_selected_page_notify({
        let app = app.clone();
        move |_| app.sync_active()
    });

    // Dark mode and the accent colour are pure GNOME settings; we only re-colour our text tags.
    let style = adw::StyleManager::default();
    for prop in ["accent-color", "dark"] {
        style.connect_notify_local(Some(prop), {
            let app = app.clone();
            move |_, _| {
                for tab in app.open.borrow().values() {
                    tab.restyle();
                }
            }
        });
    }
    style.connect_document_font_name_notify(|_| install_document_font());

    window.connect_map(|_| tracing::debug!(t_ms = ms(), "window mapped"));
    window.present();
    tracing::debug!(t_ms = ms(), "window presented");
    if let Some(rel) = note {
        app.open_note(&rel);
    }
    install_bench_hooks(&app);
    start_reconcile(&app, db);
}

// ----------------------------------------------------------------------------------- benchmarks

/// `ACCENT_BENCH_EXPAND=<rel_path>` and `ACCENT_BENCH_SWITCHER=<query>` time the two interactions
/// that used to stall the main loop, print the numbers to stdout and quit. Both run headless under
/// Xvfb, so "expanding a big directory is still fast" stays a command anyone can re-run rather
/// than a claim in a commit message. `RUST_LOG=accent=debug` adds the per-query breakdown.
fn install_bench_hooks(app: &Rc<App>) {
    let expand = std::env::var("ACCENT_BENCH_EXPAND").ok();
    let switcher = std::env::var("ACCENT_BENCH_SWITCHER").ok();
    if expand.is_none() && switcher.is_none() {
        return;
    }
    let app = app.clone();
    // After the first frame, so widget realisation is not counted in the numbers.
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        if let Some(rel) = expand {
            bench_expand(&app, &rel);
        }
        let Some(query) = switcher else {
            bench_quit(&app);
            return;
        };
        let t0 = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.switcher", None);
        println!("bench switcher_open_ms {:.1}", ms_since(t0));

        // A query of "1" just means "open it"; anything else is typed into the entry so the
        // debounce, the lazy corpus load and the match all get exercised.
        let entry = (query != "1")
            .then(|| find_search_entry(app.window.upcast_ref()))
            .flatten();
        let Some(entry) = entry else {
            bench_quit(&app);
            return;
        };
        let t1 = Instant::now();
        entry.set_text(&query);
        // Debounced, so the keystroke itself must return immediately.
        println!("bench switcher_keystroke_ms {:.1}", ms_since(t1));
        // Long enough for GtkSearchEntry's own ~150 ms delay plus our 50 ms debounce.
        glib::timeout_add_local_once(Duration::from_millis(1500), move || bench_quit(&app));
    });
}

/// First `GtkSearchEntry` in `w`'s subtree. The switcher dialog is hosted inside the window, so
/// the bench can drive it without a real key press (no xdotool in the headless image).
fn find_search_entry(w: &gtk::Widget) -> Option<gtk::SearchEntry> {
    if let Ok(e) = w.clone().downcast::<gtk::SearchEntry>() {
        return Some(e);
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(found) = find_search_entry(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// Closing the window is not enough to end the process while a dialog is up: quit the
/// application so the bench always terminates.
fn bench_quit(app: &Rc<App>) {
    match app.window.application() {
        Some(gtk_app) => gtk_app.quit(),
        None => app.window.close(),
    }
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn find_row(model: &gtk::TreeListModel, rel: &str) -> Option<gtk::TreeListRow> {
    (0..model.n_items()).find_map(|i| {
        let row = model.item(i).and_downcast::<gtk::TreeListRow>()?;
        let (_, r) = row.item().as_ref().and_then(tree::decode)?;
        (r == rel).then_some(row)
    })
}

fn bench_expand(app: &Rc<App>, rel: &str) {
    let Some(tree) = app.tree.get() else { return };
    let model = tree.model();
    let mut path = String::new();
    for seg in rel.split('/') {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(seg);
        let Some(row) = find_row(model, &path) else {
            println!("bench expand {path} NOT-FOUND");
            return;
        };
        let before = model.n_items();
        let t0 = Instant::now();
        row.set_expanded(true);
        println!(
            "bench expand {path} revealed {} rows in {:.1} ms",
            model.n_items().saturating_sub(before),
            ms_since(t0)
        );
    }
    // `is_expandable` is what `GtkTreeExpander::set_list_row` calls for every row the ListView
    // binds, i.e. the per-row cost paid while scrolling.
    let n = model.n_items();
    let t0 = Instant::now();
    for i in 0..n {
        if let Some(row) = model.item(i).and_downcast::<gtk::TreeListRow>() {
            let _ = row.is_expandable();
        }
    }
    println!("bench bind_probe {n} rows in {:.1} ms", ms_since(t0));
}

fn sidebar(app: &Rc<App>) -> gtk::Widget {
    let t = tree::build(app.index.clone(), &app.root, {
        let app = app.clone();
        move |kind, rel| {
            if kind == 'm' {
                app.open_note(rel);
            } else {
                app.toast("Only markdown notes open in this spike");
            }
        }
    });
    let list = t.view().clone();
    let _ = app.tree.set(t);
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&scroller);
    column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    column.append(&backlinks_pane(app));
    column.upcast()
}

fn backlinks_pane(app: &Rc<App>) -> gtk::Widget {
    let heading = gtk::Label::builder()
        .label("Backlinks")
        .xalign(0.0)
        .margin_start(12)
        .margin_top(6)
        .margin_bottom(3)
        .build();
    heading.add_css_class("heading");
    heading.add_css_class("dim-label");

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::Middle)
            .build();
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        if let (Some(label), Some(s)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) {
            label.set_text(&s.string());
        }
    });
    let view = gtk::ListView::new(
        Some(gtk::SingleSelection::new(Some(app.backlinks.clone()))),
        Some(factory),
    );
    view.add_css_class("navigation-sidebar");
    view.connect_activate({
        let app = app.clone();
        move |view, pos| {
            if let Some(s) = view
                .model()
                .and_then(|m| m.item(pos))
                .and_downcast::<gtk::StringObject>()
            {
                app.open_note(&s.string());
            }
        }
    });

    let scroller = gtk::ScrolledWindow::builder()
        .height_request(120)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&view)
        .build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&heading);
    column.append(&scroller);
    column.upcast()
}

fn content(tabs: &adw::TabView) -> gtk::Widget {
    let bar = adw::TabBar::builder().view(tabs).autohide(false).build();
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&bar);
    column.append(tabs);
    column.upcast()
}

fn menu_button() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    menu.append(Some("About accent"), Some("win.about"));
    menu.append(Some("Quit"), Some("app.quit"));
    gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main menu")
        .menu_model(&menu)
        .build()
}

fn install_actions(gtk_app: &adw::Application, app: &Rc<App>) {
    let add = |name: &str, f: Box<dyn Fn()>| {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(move |_, _| f());
        app.window.add_action(&action);
    };

    add(
        "save",
        Box::new({
            let app = app.clone();
            move || app.save_active()
        }),
    );
    add(
        "close-tab",
        Box::new({
            let app = app.clone();
            move || {
                if let Some(page) = app.tabs.selected_page() {
                    app.tabs.close_page(&page);
                }
            }
        }),
    );
    add(
        "switcher",
        Box::new({
            let app = app.clone();
            move || {
                // One indexed query (~50 rows). The borrow ends here, before any GTK call.
                let recent = app
                    .index
                    .borrow()
                    .recent_notes(RECENT_NOTES)
                    .unwrap_or_default();
                switcher::present(
                    &app.window,
                    recent,
                    {
                        let index = app.index.clone();
                        move || index.borrow().note_paths().unwrap_or_default()
                    },
                    {
                        let app = app.clone();
                        move |rel| app.open_note(rel)
                    },
                );
            }
        }),
    );
    add(
        "about",
        Box::new({
            let app = app.clone();
            move || {
                let about = adw::AboutDialog::builder()
                    .application_name("accent")
                    .application_icon(APP_ID)
                    .version(env!("CARGO_PKG_VERSION"))
                    .developer_name("stroblme")
                    .license_type(gtk::License::Gpl30)
                    .website("https://github.com/stroblme/accent")
                    .comments("Markdown and PDF knowledge editor.")
                    .build();
                about.present(Some(&app.window));
            }
        }),
    );

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let gtk_app = gtk_app.clone();
        move |_, _| gtk_app.quit()
    });
    gtk_app.add_action(&quit);

    for (action, accel) in [
        ("win.save", "<Control>s"),
        ("win.switcher", "<Control>p"),
        ("win.close-tab", "<Control>w"),
        ("app.quit", "<Control>q"),
    ] {
        gtk_app.set_accels_for_action(action, &[accel]);
    }
}

// ---------------------------------------------------------------------------------- indexing

enum Msg {
    Progress(Progress),
    Done(Box<Result<ReconcileStats, String>>),
}

/// Reconcile on a plain `std::thread` with its own SQLite connection (WAL lets the UI keep reading)
/// and poll the channel from the main loop.
///
/// ponytail: a 120 ms poll instead of wiring an `async-channel` into the GLib context. One timeout
/// source, no extra dependency, and the latency is below what a progress label needs.
fn start_reconcile(app: &Rc<App>, db: PathBuf) {
    let (tx, rx) = mpsc::channel::<Msg>();
    let vault = app.vault.clone();
    std::thread::spawn(move || {
        let result = Index::open(&db).and_then(|mut ix| {
            ix.reconcile(&vault, |p| {
                let _ = tx.send(Msg::Progress(p));
            })
        });
        let _ = tx.send(Msg::Done(Box::new(result.map_err(|e| e.to_string()))));
    });

    app.status.set_label("Indexing…");
    app.status.set_visible(true);
    let app = app.clone();
    glib::timeout_add_local(Duration::from_millis(120), move || {
        let mut latest = None;
        for msg in rx.try_iter() {
            match msg {
                Msg::Progress(p) => latest = Some(p),
                Msg::Done(result) => {
                    app.status.set_visible(false);
                    match *result {
                        Ok(stats) => {
                            tracing::debug!(
                                t_ms = ms(),
                                scanned = stats.scanned,
                                unchanged = stats.unchanged,
                                "reconcile done"
                            );
                            if let Some(t) = app.tree.get() {
                                t.refresh();
                            }
                            app.sync_active();
                            app.toast(&format!(
                                "Indexed {} files ({} new, {} updated)",
                                stats.scanned, stats.added, stats.updated
                            ));
                        }
                        Err(e) => {
                            tracing::debug!(t_ms = ms(), error = %e, "reconcile failed");
                            app.toast(&format!("Indexing failed: {e}"));
                        }
                    }
                    return glib::ControlFlow::Break;
                }
            }
        }
        if let Some(p) = latest {
            app.status
                .set_label(&format!("Indexing… {}/{} files", p.done, p.total));
        }
        glib::ControlFlow::Continue
    });
}

// -------------------------------------------------------------------------------------- font

/// The editor uses GNOME's *document* font, not the monospace one: notes are prose.
fn install_document_font() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let desc =
        pango::FontDescription::from_string(&adw::StyleManager::default().document_font_name());
    let family = desc
        .family()
        .map(|f| f.to_string())
        .unwrap_or_else(|| "Cantarell".to_string());
    let size = match desc.size() as f64 / pango::SCALE as f64 {
        s if s > 0.0 => s,
        _ => 11.0,
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(
        "textview.accent-doc {{ font-family: \"{family}\"; font-size: {size}pt; }}"
    ));
    // ponytail: each font change adds a provider instead of replacing the previous one. Font
    // changes are rare and later providers win; swap for a stored provider if that stops holding.
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
