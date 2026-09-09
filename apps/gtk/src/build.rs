//! Building a window: the widgets, the sidebar, the Git pane and the file operations, plus the
//! display-wide font, icons and CSS every window shares.

use super::*;

pub fn build_window(
    gtk_app: &adw::Application,
    shell: &Rc<Shell>,
    root: Option<PathBuf>,
    note: Option<String>,
) -> Option<Rc<App>> {
    install_document_font();
    install_icons();
    install_chrome_css();
    theme::apply(shell.config.borrow().theme);

    // No root is a window opened on a file: no index to build, no watcher to run, and nothing
    // to add to the recent-vaults list. A root that is an `ssh://` address is a vault on another
    // machine — it opens the same way and returns just as fast, because the connection is made on
    // a thread and reports itself through the events like the indexing does.
    let (vault, events) = match &root {
        Some(root) => {
            let vault_config = shell.config.borrow().vault(root);
            let opened = match ssh::is_remote_path(root) {
                true => Vault::open_remote(&root.to_string_lossy(), vault_config),
                false => Vault::open(root, vault_config),
            };
            match opened {
                Ok((vault, events)) => {
                    vault.set_ghost(shell.config.borrow().ghost_text);
                    (Some(Arc::new(vault)), Some(events))
                }
                Err(e) => {
                    eprintln!("cannot open {}: {e:#}", root.display());
                    return None;
                }
            }
        }
        None => (None, None),
    };
    // Touched now, so the window title and any picker opened in this window read the list the
    // way it will be written; the write itself waits for the post-present idle below, an fsync
    // being no part of building a widget tree.
    if let Some(root) = &root {
        shell.config.borrow_mut().touch_recent(root);
    }

    let vault_name = match &root {
        Some(root) => root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string()),
        None => "Accent".to_string(),
    };
    // A remote window says which machine it is on, under the vault's name. Nothing else in the
    // chrome differs: it is the same vault, and the point is that it behaves like one.
    let host = root
        .as_deref()
        .and_then(|r| ssh::parse(&r.to_string_lossy()).ok())
        .map(|url| url.host)
        .unwrap_or_default();
    let title = adw::WindowTitle::new(&vault_name, &host);
    // What the task manager and the window switcher show, which is the one place a window has to
    // be told apart from the others: the vault, and the machine it is on when that is not this
    // one. Fixed for the life of the window — the header bar is where the open file is named.
    let window_title = match host.is_empty() {
        true => vault_name.clone(),
        false => format!("{vault_name} ({host})"),
    };
    let first = Pane::new(&tab_menu());
    let toasts = adw::ToastOverlay::new();
    // Hidden until something goes wrong with a connection, which for a local vault is never.
    let connection = adw::Banner::builder().button_label("Reconnect").build();
    // Hidden until the first `Progress`, so a warm start that never reports one never shows it.
    // Going visible costs the content 4 px once, at the moment indexing ends; a `GtkRevealer`
    // would slide it away instead if that ever reads as a jump.
    let statusbar = statusbar::Bar::new();

    // An empty vault window should say so rather than showing a blank rectangle.
    let placeholder = adw::StatusPage::builder()
        .icon_name("text-x-generic-symbolic")
        .title("No Note Open")
        .description("Pick one in the sidebar, or press Ctrl+E to go to a file.")
        .build();
    // The panes hang off a bin, so a split can swap the whole arrangement for a `GtkPaned` the
    // same way it swaps one branch of it (`panes::split`).
    let panes_root = adw::Bin::builder().child(first.widget()).build();
    let content = gtk::Stack::new();
    content.add_named(&panes_root, Some("tabs"));
    content.add_named(&placeholder, Some("empty"));
    content.set_visible_child_name("empty");

    // Editor on the left, preview on the right; the mode decides which of the two is visible.
    let paned = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&content)
        .resize_start_child(true)
        .resize_end_child(true)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .build();

    toasts.set_child(Some(&paned));

    // Split headers, as GNOME Files and VS Code have them: the sidebar is a full-height column
    // with a header of its own, and the tab bar belongs to the editor column. The two header
    // bars share the window controls so they still read as one titlebar.
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_show_start_title_buttons(true);
    sidebar_header.set_show_end_title_buttons(false);
    // `build_sidebar` makes the pane switcher this header's title widget. Until then a label keeps
    // the header from falling back to the window title, which here is the application name next to
    // the vault name on the right.
    sidebar_header.set_title_widget(Some(&gtk::Label::new(None)));
    // libadwaita gives a header that shares its toolbar area with another bar 3 px of padding and
    // the bar area another 3, but a lone header keeps the default 6 above and 7 below. This one is
    // alone in its column while the main header sits above the tab bar, so without the correction
    // in `install_chrome_css` the two headers hold their contents in bands of different heights.
    sidebar_header.add_css_class("accent-lone-header");

    let sidebar_column = adw::ToolbarView::builder().width_request(200).build();
    sidebar_column.add_top_bar(&sidebar_header);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    // The toggle belongs to the header that never goes away, as in Files and Text Editor: in the
    // sidebar's own header, hiding the sidebar takes the way back with it. An icon toggle in a
    // header bar is already flat, so it reads as one family with the view-mode group at the other
    // end of the bar without a style class of its own.
    let toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Toggle Sidebar")
        .valign(gtk::Align::Center)
        .build();
    header.pack_start(&toggle);
    toggle
        .bind_property("active", &sidebar_column, "visible")
        .bidirectional()
        .sync_create()
        .build();
    toggle.set_active(true);
    // The start window controls sit in the sidebar header, so the main header takes them over
    // while the sidebar is hidden. Nothing moves on the default GNOME layout, where that side is
    // empty; a user who keeps buttons on the left does not lose them.
    sidebar_column
        .bind_property("visible", &header, "show-start-title-buttons")
        .invert_boolean()
        .sync_create()
        .build();
    let modes = mode_switcher();
    // Split View keeps its action, its chord and its place in the primary menu; what it loses is
    // the header button, whose place the drawing tools take. The widget stays because the window
    // still reads and writes its pressed state.
    let drawing = gtk::ToggleButton::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text(label_of("win.pdf-draw"))
        .action_name("win.pdf-draw")
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    drawing.add_css_class("flat");
    let menu = menu_button();
    header.pack_end(&menu);
    header.pack_end(&drawing);

    // The two headers must end at the same height or the switcher row and the tab bar under them
    // cannot line up. They do at the default font (both 40 px), but the sidebar header is empty
    // and stays at Adwaita's minimum while this one grows with the window title: measured at
    // 20 pt, the switcher row starts 14 px above the tab bar without this. The widgets keep the
    // group alive.
    let headers = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
    headers.add_widget(&sidebar_header);
    headers.add_widget(&header);

    // The panes' tab bars carry the fade class too, so the chrome still hides as one
    // (DESIGN.md); `Pane::new` adds it to each of them.
    sidebar_header.add_css_class("chrome-fade");
    header.add_css_class("chrome-fade");

    // A first connection to a remote host is the window becoming usable, not a list being
    // replaced, so its bar spans the document column rather than sitting in the status bar
    // beside the text (DESIGN.md, Loading). The text stays in the status bar either way. Only a
    // remote vault puts the widget in the layout: a local one is connected from the moment it
    // opens, so there is nothing to draw and no height to reserve.
    let connect = connect::Bar::new();
    let editor_column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    if vault.as_ref().is_some_and(|v| v.is_remote()) {
        editor_column.append(connect.widget());
    }
    editor_column.append(&toasts);

    // Only the header is a top bar now: the tab bars belong to the panes, so they sit inside
    // `content` and presentation mode takes them away with it rather than unrevealing them.
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.add_top_bar(&connection);
    // A bottom bar rather than a row inside the content: presentation mode takes it away with the
    // header for one line, and the find bar and the terminal panel stack above it.
    toolbar.add_bottom_bar(statusbar.widget());
    toolbar.set_content(Some(&editor_column));

    // One flat background across sidebar, chrome and document (DESIGN.md, Colour): without it
    // the two columns sit on `--window-bg-color` and band against the note. The header bars and
    // the tab bar need nothing of their own; `AdwToolbarView` draws its top bars flat by default,
    // so they take the colour set here.
    sidebar_column.add_css_class("accent-flat");
    toolbar.add_css_class("accent-flat");

    // ponytail: a plain `GtkPaned` so the sidebar can be dragged, which `AdwOverlaySplitView`
    // cannot do. The cost is its adaptive collapse on a narrow window; go back to it if that
    // ever matters more than resizing.
    let split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .start_child(&sidebar_column)
        .end_child(&toolbar)
        .resize_start_child(false)
        .shrink_start_child(false)
        .resize_end_child(true)
        .position(Session::default().sidebar_width)
        .build();

    let window = adw::ApplicationWindow::builder()
        .application(gtk_app)
        .title(&window_title)
        .default_width(1100)
        .default_height(760)
        .content(&split)
        .build();

    let app = Rc::new(App {
        vault: vault.clone(),
        // (`vault` is an `Option` here: `None` is a window opened on a file, with no folder.)
        shell: Rc::downgrade(shell),
        config: shell.config.clone(),
        window: window.clone(),
        panes: RefCell::new(vec![first.clone()]),
        active_pane: RefCell::new(first.clone()),
        title,
        toasts,
        connection,
        connect,
        corpus: RefCell::new(Corpus::default()),
        statusbar,
        docs: RefCell::new(Vec::new()),
        awaiting: RefCell::new(HashMap::new()),
        tree: OnceCell::new(),
        sidebar: OnceCell::new(),
        git: OnceCell::new(),
        excluded: RefCell::new(None),
        references: RefCell::new(None),
        ops: OnceCell::new(),
        preview: RefCell::new(None),
        terminals: Cell::new(0),
        split,
        sidebar_column,
        sidebar_header,
        toolbar,
        editor_column,
        header,
        modes: modes.clone(),
        drawing_button: drawing.clone(),
        drawing: Cell::new(false),
        tool: Cell::new(pdfview::Mode::Pen),
        ring_at: Cell::new(None),
        menu,
        paned,
        content: content.clone(),
        mode: Cell::new(Mode::Editor),
        zoom: Cell::new(1.0),
        presenting: Cell::new(None),
        chrome_hidden: Cell::new(false),
        navigating: Cell::new(false),
        reconciled: Cell::new(false),
        menu_page: RefCell::new(None),
        tree_painted: Cell::new(0),
        refresh: RefCell::new(None),
        pdf_links: RefCell::new(None),
        session: RefCell::new(None),
        recent_notes: RefCell::new(Vec::new()),
        recent_commands: RefCell::new(Vec::new()),
        captured: gtk::ShortcutController::new(),
    });
    if let Some(vault) = &vault {
        let _ = app.ops.set(build_ops(&app, vault));
    }

    // The sidebar is the vault: a tree, a search over the index, the tags in it, the backlinks
    // between its notes. A window without one is tabs and nothing else.
    match &vault {
        Some(vault) => {
            // Populate straight from the index: the window must be up before reconcile finishes.
            let rows = gio::ListStore::new::<gtk::StringObject>();
            tree::fill(&rows, vault, "");
            tracing::debug!(t_ms = ms(), rows = rows.n_items(), "tree populated");
            build_sidebar(&app, &rows, vault);
        }
        // Outline only, and collapsed: a window opened on one file is that file, and the
        // sidebar is there for when it is asked for with F9 or Ctrl+Shift+L.
        None => {
            build_outline_sidebar(&app);
            app.sidebar_column.set_visible(false);
        }
    }

    wire_pane(&app, &first);

    install_actions(&app);
    wire_window(&app, &modes);
    if vault.is_some() {
        wire_tree(&app);
    }

    window.connect_map(|_| tracing::debug!(t_ms = ms(), "window mapped"));
    window.present();
    tracing::debug!(t_ms = ms(), "window presented");

    glib::idle_add_local_once(glib::clone!(
        #[weak]
        app,
        move || {
            // Before the save below, which is what rewrites the keys it reads out of the file.
            retired_daily_keys(&app);
            // The recent list, written once the window the user asked for is on screen.
            if app.vault().is_some()
                && let Err(e) = app.config.borrow().save()
            {
                tracing::warn!("saving config: {e:#}");
            }
            app.restore_session();
            if let Some(rel) = note {
                app.open_path(&rel);
            }
        }
    ));
    bench::install_bench_hooks(&app);
    if let Some(events) = events {
        start_events(&app, events);
    }
    Some(app)
}

/// The sidebar for a window with a vault: the tree, the index panes and the outline.
fn build_sidebar(app: &Rc<App>, rows: &gio::ListStore, vault: &Arc<Vault>) {
    let tree = tree::build(
        vault.clone(),
        rows,
        // The row's kind used to decide what opened. `open_path` reads the name itself, so the
        // tree no longer has to agree with it about what a file is. A row opens as a preview:
        // one click is looking, not keeping.
        glib::clone!(
            #[weak]
            app,
            move |_kind, rel: &str| app.open_preview(rel)
        ),
        // A drag out of the tree is the only notice the panes get that their drop zones should
        // go up; a tab drag announces itself through `AdwTabView:is-transferring-page`.
        glib::clone!(
            #[weak]
            app,
            move |on| app.set_drop_active(on)
        ),
        // A row dropped back into the tree moves the file. Where it may land at all is decided
        // before the drop; what is left is the same plan-and-rewrite Rename goes through.
        glib::clone!(
            #[weak]
            app,
            move |from: &str, to: &str| {
                if let Some(ops) = app.ops() {
                    fileops::move_dropped(ops, from, to);
                }
            }
        ),
    );
    // The tree owns its scroller now, wrapped in a box the context menu can parent itself to.
    let files = tree.widget().clone();
    let _ = app.tree.set(tree);

    let data = sidebar::Data {
        search: search_data(app, vault),
        tags: tags_data(vault),
        ports: ports_data(vault),
    };
    let git = build_git(app, vault);
    adopt_sidebar(
        app,
        Some((files, data, git.widget().clone(), git.divider().clone())),
    );
    let _ = app.git.set(git);
    if let Some(git) = app.git.get() {
        git.schedule_refresh(git::Depth::Discover);
    }
}

/// What the Search pane asks of the index.
fn search_data(app: &Rc<App>, vault: &Arc<Vault>) -> sidebar::SearchData {
    sidebar::SearchData {
        // The one closure the sidebar calls off the main loop, which is why the vault is an `Arc`.
        search: Arc::new({
            let vault = vault.clone();
            move |query| match query {
                sidebar::Query::Fts(text, all) => {
                    sidebar::Answer::Fts(vault.search(&text, SEARCH_LIMIT, all).unwrap_or_default())
                }
                sidebar::Query::Grep { text, options, all } => {
                    // `total` is what Replace All would rewrite, not how many rows there are:
                    // the walked trees below add rows and nothing to it, and neither does a
                    // source file the index holds a body for. The button promises edits.
                    let (mut hits, total) = vault
                        .grep(&text, options, SEARCH_LIMIT, all)
                        .unwrap_or_default();
                    // What the index holds first, because that is what it can count; with
                    // All on, the trees it was never asked to hold get whatever room is left.
                    if all {
                        let room = SEARCH_LIMIT.saturating_sub(hits.len());
                        hits.extend(
                            vault
                                .grep_unindexed(&text, options, room)
                                .unwrap_or_default(),
                        );
                    }
                    sidebar::Answer::Grep(hits, total)
                }
            }
        }),
        replace_all: Box::new(glib::clone!(
            #[weak]
            app,
            move |query: String,
                  options: accent_api::Options,
                  replacement: String,
                  literal: bool,
                  done: Box<dyn FnOnce()>| {
                app.replace_in_notes(query, options, replacement, literal, done)
            }
        )),
    }
}

/// What the Tags pane asks of the index.
fn tags_data(vault: &Arc<Vault>) -> sidebar::TagsData {
    sidebar::TagsData {
        tags: Arc::new({
            let vault = vault.clone();
            move || vault.tags().unwrap_or_default()
        }),
        files_with_tag: Arc::new({
            let vault = vault.clone();
            move |tag| {
                vault
                    .files_with_tag(tag)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|f| f.rel_path)
                    .collect()
            }
        }),
    }
}

/// Port forwarding is ssh's, over the master that is already open: nothing is spawned and nothing
/// is kept but the list the pane shows.
fn ports_data(vault: &Arc<Vault>) -> sidebar::PortsData {
    sidebar::PortsData {
        add_forward: Arc::new({
            let vault = vault.clone();
            move |local, remote| match vault.remote() {
                Some(r) => r.forward(local, remote),
                None => Err("this vault is not remote".to_string()),
            }
        }),
        remove_forward: Box::new({
            let vault = vault.clone();
            move |local, remote| {
                if let Some(r) = vault.remote()
                    && let Err(e) = r.cancel_forward(local, remote)
                {
                    tracing::warn!("cancelling the forward {local} -> {remote}: {e}");
                }
            }
        }),
    }
}

/// The Git pane. Every hook holds the window weakly: the pane lives in the sidebar, which the
/// window owns, so a strong capture here is a cycle that keeps a closed window's vault open.
fn build_git(app: &Rc<App>, vault: &Arc<Vault>) -> Rc<git::Panel> {
    let (toast, open, diff, compare, trash, changed, syncing, set_tree) = (
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
        Rc::downgrade(app),
    );
    git::Panel::new(git::Hooks {
        tree: app.config.borrow().git_tree,
        vault: vault.clone(),
        window: app.window.clone(),
        toast: Box::new(move |text| {
            if let Some(app) = toast.upgrade() {
                app.toast(text);
            }
        }),
        open: Box::new(move |key| {
            if let Some(app) = open.upgrade() {
                // A single click, the same as a tree row, so the same preview tab.
                app.open_preview(key);
            }
        }),
        open_diff: Box::new(move |key, file, title, old, new| {
            diff.upgrade()
                .map(|app| app.open_diff(key, file, title, old, new))
        }),
        compare_file: Box::new(move |key, title, text, register| {
            let Some(app) = compare.upgrade() else {
                return;
            };
            let (title, text) = (title.to_string(), text.to_string());
            // The file's own tab, as a preview like any other single click in the sidebar.
            app.with_tab(key, Opened::Preview, move |_, tab| {
                let name = doc::file_name(&tab.rel()).to_string();
                let compare = tab.compare(
                    &format!("{name} (Working Tree)"),
                    (&title, &text),
                    diff::Side::New,
                    false,
                    None,
                    "Working Tree",
                );
                register(Rc::downgrade(&compare));
            });
        }),
        trash: Box::new(move |key| {
            if let Some(ops) = trash.upgrade().and_then(|app| app.ops().cloned()) {
                fileops::trash(&ops, key);
            }
        }),
        changed: Box::new(move || {
            if let Some(app) = changed.upgrade() {
                app.on_git_changed();
            }
        }),
        syncing: Box::new(move |on| {
            if let Some(app) = syncing.upgrade() {
                app.statusbar.set_syncing(on);
            }
        }),
        // The pane has already redrawn itself; this only writes the preference the Preferences
        // switch edits, so the two surfaces stay one value.
        set_tree: Box::new(move |on| {
            let Some(app) = set_tree.upgrade() else {
                return;
            };
            app.config.borrow_mut().git_tree = on;
            settings::save(&app.config.borrow());
        }),
    })
}

/// A sidebar with the Outline pane alone, for a window opened on a file rather than a folder.
/// There is no index behind it, so Files, Search, Tags and References have nothing to show; an
/// outline does not need one, and a PDF's bookmarks are the reason such a window has a sidebar.
fn build_outline_sidebar(app: &Rc<App>) {
    adopt_sidebar(app, None);
}

fn adopt_sidebar(
    app: &Rc<App>,
    vault: Option<(gtk::Widget, sidebar::Data, gtk::Widget, gtk::Paned)>,
) {
    let pane = sidebar::Sidebar::new(
        vault,
        glib::clone!(
            #[weak]
            app,
            move |rel: &str, at: Option<Range<usize>>| app.open_note_at(rel, at)
        ),
        glib::clone!(
            #[weak]
            app,
            move |row: &str| {
                if let Some(loc) = reference_target(row) {
                    app.open_at(&loc);
                }
            }
        ),
    );
    // The switcher is the sidebar header's title widget rather than a top bar of its own, so the
    // sidebar spends no band on an empty header: the pane icons sit level with the collapse toggle
    // in the main header, and the tree starts level with the tab bar. `AdwHeaderBar` centres a
    // title widget, and the header-to-header size group already keeps the two bands equal, so the
    // switcher needs neither a box around it nor a size group of its own.
    // A remote vault is the only one with ports to forward, and that is settled when the window
    // is built rather than discovered later, so unlike the Git pane this needs no refresh to say.
    pane.set_ports_visible(app.vault().is_some_and(|v| v.is_remote()));
    app.sidebar_header.set_title_widget(Some(pane.switcher()));
    // The panes dim rather than hide while the user types, on the same transition as the bars.
    pane.widget().add_css_class("chrome-fade");
    app.sidebar_column.set_content(Some(pane.widget()));
    let _ = app.sidebar.set(pane);
}

/// Say once, in the window and in the log, what a vault's retired `daily_*` settings mean now.
///
/// The keys are gone, so a vault configured for a daily note would simply stop making one with
/// nothing saying why. Nothing is written into the vault — the `accent-target:` line is the
/// user's to add, and a template travels with the vault where these keys never did — and the
/// keys leave `config.toml` with the caller's next save, so this is said once. A courtesy: a
/// config that will not re-read says nothing at all.
fn retired_daily_keys(app: &Rc<App>) {
    let Some(root) = app.vault().map(|v| v.root()) else {
        return;
    };
    let Some((target, template)) = accent_core::config::daily_keys(&root) else {
        return;
    };
    let dir = app.config.borrow().vault(&root).templates_dir;
    let file = match template {
        Some(t) => accent_core::template::candidates(&dir, &t)
            .pop()
            .unwrap_or(t),
        None => format!("a template in {dir}"),
    };
    let say = format!(
        "Daily notes are a template directive now: add `accent-target: {target}` to {file}"
    );
    tracing::warn!("{say}");
    app.toast(&say);
}

/// Everything `fileops` needs from the window, as closures. Weak throughout: the operations
/// outlive nothing, and a strong capture here would keep a closed window's vault open.
fn build_ops(app: &Rc<App>, vault: &Arc<Vault>) -> Rc<fileops::Ops> {
    let toast = Rc::downgrade(app);
    let open = Rc::downgrade(app);
    let flush = Rc::downgrade(app);
    let reload = Rc::downgrade(app);
    let close = Rc::downgrade(app);
    let reconciled = Rc::downgrade(app);
    let exclude = Rc::downgrade(app);
    Rc::new(fileops::Ops {
        vault: vault.clone(),
        window: app.window.clone(),
        toast: Box::new(move |message| {
            if let Some(app) = toast.upgrade() {
                app.toast(message);
            }
        }),
        open: Box::new(move |rel, stops| {
            let Some(app) = open.upgrade() else { return };
            let stops = stops.to_vec();
            app.with_tab(rel, Opened::Kept, move |_, tab| tab.place_stops(&stops));
        }),
        reconciled: Box::new(move || reconciled.upgrade().is_some_and(|app| app.reconciled.get())),
        flush: Box::new(move |rels| {
            let Some(app) = flush.upgrade() else { return };
            // Each path is a subtree and not only a key: a folder on its way to the trash or into
            // another folder takes every note under it, and their buffers have to be written out
            // before the file moves. `trashed_with` is the same "is under" the close path asks.
            for tab in app.open_tabs() {
                let under = rels
                    .iter()
                    .any(|rel| fileops::trashed_with(rel, &tab.rel()));
                if under && tab.modified.get() {
                    app.save_tab(&tab, false);
                }
            }
        }),
        reload: Box::new(move |rels| {
            let Some(app) = reload.upgrade() else {
                return 0;
            };
            rels.iter()
                .filter_map(|rel| app.tab_for(rel))
                .filter(|tab| !app.refresh_tab(tab))
                .count()
        }),
        exclude: Box::new(move |dir| {
            let Some(app) = exclude.upgrade() else { return };
            {
                let mut config = app.config.borrow_mut();
                if config.search.exclude.iter().any(|e| e == dir) {
                    return app.toast(&format!("{dir} is already left out of search"));
                }
                config.search.exclude.push(dir.to_string());
            }
            settings::save(&app.config.borrow());
            // The one path that turns git's answer and this list into the set the tree dims and
            // the index leaves out. Called whole rather than half-copied here; what it repeats on
            // top of the exclusions is a branch readout and a `git show` per tab.
            app.on_git_changed();
            app.toast(&format!("Left {dir} out of search"));
        }),
        close: Box::new(move |rel| {
            let Some(app) = close.upgrade() else { return };
            // A folder in the trash takes everything under it, so every document at or below the
            // path goes with it — a PDF or an image as much as a note. The file is in the trash:
            // there is nothing left to save a buffer into, so the tab goes without the close
            // asking to write it back out again, and unsaved edits go with the file.
            for doc in app.docs() {
                if !fileops::trashed_with(rel, &doc.key()) {
                    continue;
                }
                if let Some(tab) = doc.tab() {
                    tab.discard();
                }
                app.close_page(doc.page());
            }
        }),
    })
}

/// The display-wide rule every editor starts from: Adwaita Mono at the size of GNOME's *document*
/// font, which is [`editor::default_font`]. A vault is prose with code fences, tables and
/// wikilinks in it, and none of those line up in a proportional face, so the family is ours and
/// only the size follows the system.
///
/// It goes through [`editor::font_css`], the same function a tab's own zoom rule is written with,
/// so the family and the size are decided in one place and a zoomed note cannot end up in a
/// different face from an unzoomed one.
pub fn install_document_font() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&editor::font_css(
        &editor::default_font(),
        // The label too: the editor's sticky block title is a line of the document, and a tab at
        // the default zoom has no `#accent-doc-N` rule of its own for it to pick the face up from.
        "textview.accent-doc, label.accent-doc",
        1.0,
    ));
    // Replaced rather than stacked, the way `theme::apply` handles its own provider: this runs
    // once per window as well as on every font change, so adding would grow the display's
    // provider list for the life of the process.
    FONT.with_borrow_mut(|slot| {
        if let Some(old) = slot.replace(provider.clone()) {
            gtk::style_context_remove_provider_for_display(&display, &old);
        }
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}

thread_local! {
    /// The document-font provider currently on the display, so the next call can take it off.
    static FONT: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
}

/// The app's own rules. The chrome fade (DESIGN.md) is opacity only, so the layout never
/// shifts and neither the focus order nor accessibility notices; with `gtk-enable-animations` off
/// the class still toggles but there is no transition, so the chrome snaps instead of fading and
/// nothing becomes unreachable. `.accent-flat` puts the two columns on the note's own background
/// so nothing bands against it, on a class of ours rather than on `headerbar` globally.
/// `.accent-lone-header` drops the bottom padding of the sidebar header, the one header in the
/// window that does not sit above a second bar: libadwaita pads a stacked header 3 px top and
/// bottom and its bar area another 3, so with 6 above and none below both headers hold their
/// contents in the same band whatever the interface font makes of their height.
/// `.accent-bar-button` does the same job for the status bar's two controls, the branch readout
/// and the zoom one: Adwaita gives a button a 24 px minimum and 5 px of padding either side, a box
/// is as tall as its tallest child however that child is aligned, and so either of them appearing
/// lifted the bar from 29 px to 46 px. Dropping the minimum and the vertical padding puts them on
/// the caption's own line height, and they stay buttons rather than becoming labels, so the click,
/// the focus ring and the tooltip stay.
///
/// The last rules are corrections to GtkSourceView, which styles itself from its style scheme
/// (a widget-level provider at priority 598) and from its own CSS (599). A display provider at
/// `STYLE_PROVIDER_PRIORITY_APPLICATION` outranks both per property, so the document takes the
/// theme's view colours instead of the scheme's grey, and the completion popup takes the
/// popover's. The scheme itself stays: dropping it takes the find bar's match highlight with it.
/// On the `text` node only `color` is ours, because GtkSourceView pins that node's background to
/// transparent at maximum priority; the background therefore goes on the `textview` node. The
/// gutter is the same correction one node over: a scheme's `line-numbers` style carries a
/// background of its own, and Solarized's is a shade off its text background (`base2` on `base3`,
/// `base02` on `base03`), so the line numbers sat in a stripe beside the page. Adwaita happens to
/// paint the two the same, which is why only Solarized showed it.
// ponytail: the header rule leans on libadwaita's own header padding (6 above a lone header,
// 3 + 3 above a stacked one) adding up to the same offset. Reach for `AdwToolbarView`'s spacing
// API instead if one ever appears; today the class is the only handle on it.
//
// A handle under the pointer takes the accent colour without changing size, so it says it can be
// dragged before it is. `box-shadow: none` is what makes it visible at all: Adwaita draws the line
// as a 1 px inset shadow over a transparent background, and on a 1 px handle that shadow covers
// the whole allocation, so a background colour alone would never show. The dragging rule above
// paints 3 px, of which the shadow still covers one; the hover rule follows it, so a handle being
// dragged is the same colour as one being aimed at and only the width changes.
//
// ponytail: `paned.dragging` widens the handle from 1 px to 3 px, which moves the pane beside it
// by 2 px for the length of the drag. Drawing outside the 1 px allocation instead, with an
// outline or a negative margin, was measured: it only ever reaches the side rendered before the
// handle, because the pane after it paints over the other. A 2 px shift while a divider is being
// dragged is invisible, so it is the cheaper of the two.
/// Registers the icons compiled into the binary and points the theme at them.
///
/// A GResource rather than hicolor: the completion list needs the kind icons long before anyone
/// runs `make install`, and the theme keeps answering for every Adwaita name as it did.
fn install_icons() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Err(e) = gio::resources_register_include!("accent.gresource") {
            tracing::warn!("icons: {e}");
            return;
        }
        let Some(display) = gdk::Display::default() else {
            return;
        };
        gtk::IconTheme::for_display(&display).add_resource_path("/io/github/stroblme/Accent/icons");
    });
}

fn install_chrome_css() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let fade = match gtk::Settings::for_display(&display).is_gtk_enable_animations() {
            true => ".chrome-fade { transition: opacity 250ms ease; } ",
            false => "",
        };
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&format!(
            "{fade}.chrome-hidden {{ opacity: 0; }} \
             .chrome-dimmed {{ opacity: 0.5; }} \
             .accent-drop-zone {{ background-color: var(--accent-bg-color); opacity: 0.3; }} \
             .git-actions {{ opacity: 0; }} \
             row:hover .git-actions, row:focus-within .git-actions {{ opacity: 1; }} \
             .git-log > row {{ margin-top: 0; margin-bottom: 0; }} \
             popover.git-menu > contents {{ background-color: var(--popover-bg-color); }} \
             paned.dragging > separator {{ min-width: 3px; min-height: 3px; \
               background-color: var(--border-color); }} \
             paned > separator:hover {{ box-shadow: none; \
               background-color: var(--accent-bg-color); }} \
             .accent-flat, .accent-flat:backdrop {{ background-color: var(--view-bg-color); }} \
             .accent-bar-button {{ min-height: 0; padding: 0 6px; border-radius: 6px; }} \
             .accent-ring-tool, .accent-ring-hub {{ min-width: 0; min-height: 0; padding: 0; \
               box-shadow: 0 1px 4px var(--shade-color); }} \
             .accent-ring-tool:checked {{ background-color: var(--accent-bg-color); \
               color: var(--accent-fg-color); }} \
             .accent-ring-hub {{ opacity: 0.75; }} \
             .accent-lone-header > windowhandle > box {{ padding-bottom: 0; }} \
             textview.accent-doc {{ color: var(--view-fg-color); \
               background-color: var(--view-bg-color); }} \
             textview.accent-doc text {{ color: var(--view-fg-color); }} \
             textview.accent-carets text {{ caret-color: transparent; }} \
             textview border gutter {{ background-color: var(--view-bg-color); }} \
             GtkSourceAssistant {{ background-color: var(--popover-bg-color); \
               color: var(--popover-fg-color); \
               box-shadow: 0 1px 4px var(--shade-color), 0 0 0 1px var(--shade-color); }} \
             GtkSourceAssistant.completion {{ min-width: 240px; }} \
             GtkSourceAssistant.completion list row {{ padding: 3px 6px; }} \
             GtkSourceAssistant.completion list row cell.typed-text {{ margin-left: 12px; \
               margin-right: 12px; min-height: 30px; }} \
             GtkSourceAssistant.completion list row cell.icon {{ opacity: 0.7; }} \
             GtkSourceAssistant.completion list row cell.after {{ opacity: 0.6; \
               margin-left: 12px; }} \
             textview.GtkSourceMap {{ font-size: 2.5pt; line-height: 6px; }}"
        ));
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
}
