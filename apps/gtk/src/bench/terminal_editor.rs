//! `ACCENT_BENCH_EDITOR`: files opened in a terminal editor. Only `build-aux/fake-editor.sh`
//! ever runs as one, linked into a scratch `HOME`'s `bin` as `nano` (and `kak`, which ends as a
//! missing program does), which that `HOME`'s `.profile` puts first on the login shell's `PATH`.

use super::*;
use crate::terminal::Term;
use accent_core::config::Editor;
use vte4::TerminalExt as _;

/// `=prefs`: Preferences → Text Editor lists the fake `nano` once the shell has been asked, and
/// picking it is the config's editor. `=open`: notes open in it as tabs that look like their
/// files', once each; a diagram, a notebook and an image stay accent's; exit 0 closes the tab,
/// exit 1 keeps it, a missing editor hands the file to accent's own; a preview is replaced.
/// `=keys`, through XTEST with `ACCENT_FAKE_EDITOR=keys`, which logs three keys raw: `Ctrl+W`,
/// `Ctrl+E` and `Ctrl+J` reach the editor; `Ctrl+PageUp` and `Ctrl+Tab` leave it; `Ctrl+Shift+P`
/// opens the palette over it; a key keeps a preview. `=accent`: Open in Accent's Editor on a
/// preview ends its editor unasked, a kept tab's would ask (an `AdwAlertDialog` takes no answer
/// under Xvfb). `=hold`, then `=back` twice on one scratch state, the holder ended between: the
/// editor's tab comes back attached to the editor, then with the editor started again, and its
/// preview tab with it. `=line`: a
/// search hit and Go to Symbol open the editor at their line, and a hit in a file it has open
/// brings its tab to the front without starting it again; a custom command gets its file and line.
/// `=panes`: a note in the editor has a preview tab, an Outline and References of its own, which
/// follow a save made while the editor runs, pictured into `$TMPDIR`.
pub(super) fn bench_editor(app: &Rc<App>, step: &str) {
    let (app, step) = (app.clone(), step.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(400)).await;
        match step.as_str() {
            "prefs" => prefs(&app).await,
            "open" => open(&app).await,
            "keys" => keys(&app).await,
            "accent" => accent(&app).await,
            "line" => line(&app).await,
            "panes" => panes(&app).await,
            "hold" => return hold(&app).await,
            "back" => return back(&app).await,
            _ => println!("bench editor unknown step {step}"),
        }
        finish(&app);
    });
}

/// Pick `editor`, as Preferences does.
fn pick(app: &Rc<App>, editor: Editor) {
    app.config.borrow_mut().editor = editor;
    app.config_changed();
}

/// The live terminal editor on `rel`, once it has started.
async fn started(app: &Rc<App>, rel: &str) -> Option<Rc<Term>> {
    let fake = || {
        app.editor_of(rel)
            .filter(|term| text(term).contains("fake-editor"))
    };
    until("editor", &format!("started {rel}"), 10_000, || {
        fake().is_some()
    })
    .await;
    fake()
}

fn text(term: &Term) -> String {
    term.view
        .text_format(vte4::Format::Text)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

/// What kind of tab has `rel` open.
pub(super) fn kind_of(app: &Rc<App>, rel: &str) -> &'static str {
    if app.editor_of(rel).is_some() {
        return "editor";
    }
    match app.doc_for(rel) {
        Some(Doc::Text(_)) => "text",
        Some(Doc::Diagram(_)) => "diagram",
        Some(Doc::Notebook(_)) => "notebook",
        Some(Doc::Image(_)) => "image",
        Some(Doc::Loading(_)) => "loading",
        Some(_) => "other",
        None => "none",
    }
}

async fn open(app: &Rc<App>) {
    let root = app.root();
    for (rel, text) in [
        ("note.md", "# Note\n"),
        ("kept.md", "# Kept\n"),
        ("gone.md", "# Gone\n"),
        ("first.md", "# First\n"),
        ("second.md", "# Second\n"),
        ("flow.drawio", super::diagram::SAMPLE),
        (
            "book.ipynb",
            r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#,
        ),
    ] {
        std::fs::write(root.join(rel), text).unwrap();
    }
    std::fs::write(root.join("anim.gif"), crate::look::TWO_FRAMES).unwrap();
    until("editor", "open reconciled", 30_000, || app.reconciled.get()).await;
    // Listed, so the tree has a row to light for the tab.
    let listed = || {
        let tree = app.tree.get();
        tree.is_some_and(|tree| crate::tree::find_row(tree.model(), "note.md").is_some())
    };
    until("editor", "open listed", 10_000, listed).await;
    pick(app, Editor::Nano);

    app.open_path("note.md");
    let Some(term) = started(app, "note.md").await else {
        return println!("bench editor open started=false");
    };
    let row = app
        .tree
        .get()
        .and_then(|tree| tree.selected())
        .map(|row| row.rel);
    let args = text(&term)
        .lines()
        .find(|line| line.starts_with("fake-editor"))
        .map(|line| line.replace(&root.display().to_string(), "<vault>"));
    println!(
        "bench editor open title={} icon={} subtitle={} row={row:?} args={args:?}",
        term.page.title(),
        term.page
            .icon()
            .and_then(|icon| IconExt::to_string(&icon))
            .unwrap_or_default(),
        app.title.subtitle(),
    );
    app.open_path("note.md");
    glib::timeout_future(Duration::from_millis(500)).await;
    for rel in ["flow.drawio", "book.ipynb", "anim.gif"] {
        app.open_path(rel);
    }
    let kinds = || ["note.md", "flow.drawio", "book.ipynb", "anim.gif"].map(|r| kind_of(app, r));
    until("editor", "open again", 10_000, || {
        !kinds().iter().any(|k| ["none", "loading"].contains(k))
    })
    .await;
    println!(
        "bench editor open again terminals={} kinds={:?}",
        app.terminals().len(),
        kinds()
    );

    term.view.feed_child(b"0\n");
    let closed = until("editor", "exit0", 10_000, || app.terminals().is_empty())
        .await
        .is_some();
    let edited = std::fs::read_to_string(root.join("note.md")).unwrap_or_default();
    println!(
        "bench editor exit0 closed={closed} edited={}",
        edited.ends_with("edited\n")
    );

    app.open_path("kept.md");
    if let Some(term) = started(app, "kept.md").await {
        term.view.feed_child(b"1\n");
        let said = until("editor", "exit1", 10_000, || {
            text(&term).contains("[Exited")
        })
        .await
        .is_some();
        let line = text(&term)
            .lines()
            .rfind(|l| l.starts_with('['))
            .map(str::to_string);
        println!(
            "bench editor exit1 kept={} said={said} line={line:?}",
            app.terminals().len() == 1
        );
    }

    // `kak` is the fake that ends as a program the shell cannot find does.
    pick(app, Editor::Kakoune);
    let toasts = app.toasted.get();
    app.open_path("gone.md");
    until("editor", "missing", 10_000, || {
        kind_of(app, "gone.md") == "text"
    })
    .await;
    println!(
        "bench editor missing kind={} toasted={}",
        kind_of(app, "gone.md"),
        app.toasted.get() - toasts
    );

    pick(app, Editor::Nano);
    app.open_preview("first.md");
    started(app, "first.md").await;
    app.open_preview("second.md");
    started(app, "second.md").await;
    glib::timeout_future(Duration::from_millis(300)).await;
    println!(
        "bench editor preview first={} second={}",
        kind_of(app, "first.md"),
        kind_of(app, "second.md")
    );
    // Every editor still running ends, so nothing is left held behind the drill.
    end_all(app, "preview ended", b"0\n").await;
}

async fn keys(app: &Rc<App>) {
    let root = app.root();
    for rel in ["note.md", "other.md", "third.md", "fourth.md"] {
        std::fs::write(root.join(rel), "# Keys\n").unwrap();
    }
    until("editor", "keys reconciled", 30_000, || app.reconciled.get()).await;
    pick(app, Editor::Nano);
    // A tab of accent's own to leave the editor's for.
    app.pending.in_accent("other.md");
    app.open_path("other.md");
    until("editor", "keys other", 10_000, || {
        kind_of(app, "other.md") == "text"
    })
    .await;
    app.open_path("note.md");
    let Some(term) = started(app, "note.md").await else {
        return println!("bench editor keys started=false");
    };
    super::git::xtest("move 700 450; focus").await;
    term.view.grab_focus();
    glib::timeout_future(Duration::from_millis(300)).await;
    let at = crate::actions::Keys::at(gtk::prelude::GtkWindowExt::focus(&app.window));
    super::git::xtest("key ctrl+w; key ctrl+e; key ctrl+j").await;
    let log = glib::home_dir().join("editor-keys");
    let read = || std::fs::read_to_string(&log).unwrap_or_default();
    until("editor", "keys bytes", 5000, || !read().trim().is_empty()).await;
    println!("bench editor keys at={at:?} bytes={}", read().trim());

    let front = || {
        let page = app.tabs().selected_page();
        page.map(|page| page.title().to_string())
            .unwrap_or_default()
    };
    let mut lines = Vec::new();
    for chord in ["ctrl+Page_Up", "ctrl+Tab"] {
        app.open_path("third.md");
        let Some(term) = started(app, "third.md").await else {
            return println!("bench editor keys third=false");
        };
        term.view.grab_focus();
        glib::timeout_future(Duration::from_millis(200)).await;
        super::git::xtest(&format!("key {chord}")).await;
        lines.push(format!("{chord}={}", front()));
    }
    app.open_path("third.md");
    if let Some(term) = started(app, "third.md").await {
        term.view.grab_focus();
    }
    glib::timeout_future(Duration::from_millis(200)).await;
    super::git::xtest("key ctrl+shift+p").await;
    let palette = app.window.visible_dialog();
    lines.push(format!("palette={}", palette.is_some()));
    if let Some(dialog) = palette {
        dialog.close();
    }
    glib::timeout_future(Duration::from_millis(300)).await;

    app.open_preview("fourth.md");
    if let Some(term) = started(app, "fourth.md").await {
        term.view.grab_focus();
        glib::timeout_future(Duration::from_millis(200)).await;
        super::git::xtest("key a").await;
        let preview = app.pane().preview();
        lines.push(format!("kept={}", preview.as_ref() != Some(&term.page)));
    }
    println!("bench editor keys {}", lines.join(" "));
    // Three keys end each fake still waiting.
    end_all(app, "keys ended", b"xyz").await;
}

async fn accent(app: &Rc<App>) {
    let root = app.root();
    std::fs::write(root.join("look.md"), "# Look\n").unwrap();
    until("editor", "accent reconciled", 30_000, || {
        app.reconciled.get()
    })
    .await;
    pick(app, Editor::Nano);
    app.open_preview("look.md");
    let Some(term) = started(app, "look.md").await else {
        return println!("bench editor accent started=false");
    };
    let preview_asks = app.editor_asks(&term.page).is_some();
    app.promote(&term.page);
    let kept_asks = app.editor_asks(&term.page).is_some();
    // Back to a preview, whose editor Open in Accent's Editor ends without a question.
    if let Some(pane) = app.pane_of(&term.page) {
        pane.set_preview(&term.page);
    }
    let _ = WidgetExt::activate_action(&app.window, "win.open-in-accent", None);
    until("editor", "accent kind", 10_000, || {
        kind_of(app, "look.md") == "text"
    })
    .await;
    println!(
        "bench editor accent preview_asks={preview_asks} kept_asks={kept_asks} kind={} editors={}",
        kind_of(app, "look.md"),
        app.terminals().len()
    );
}

/// The arguments the fake editor in `term` was started with, the vault's root written `<vault>`.
fn args(app: &Rc<App>, term: &Term) -> Option<String> {
    let root = app.root().display().to_string();
    let line = text(term)
        .lines()
        .find(|l| l.starts_with("fake-editor"))?
        .to_string();
    Some(line.replace(&root, "<vault>"))
}

async fn line(app: &Rc<App>) {
    let root = app.root();
    std::fs::write(root.join("deep.md"), "# Deep\n\nthe zebrafish swims\n").unwrap();
    std::fs::write(root.join("code.rs"), "\nfn alpha() {}\n").unwrap();
    std::fs::write(root.join("custom.md"), "# Custom\n\nline two\n").unwrap();
    until("editor", "line reconciled", 30_000, || app.reconciled.get()).await;
    pick(app, Editor::Nano);
    let hit = sidebar::Target::Range(12..21, Some(3));
    app.open_note_at("deep.md", Some(hit));
    let deep = started(app, "deep.md").await;
    let hit = deep.as_ref().and_then(|term| args(app, term));
    let symbol = accent_api::SymbolName {
        name: "alpha".to_string(),
        kind: accent_api::SymbolKind::Function,
        container: None,
        rel_path: "code.rs".to_string(),
        line: 2,
        end_line: 2,
    };
    app.goto_symbol(&symbol);
    let code = started(app, "code.rs").await;
    let symbol = code.as_ref().and_then(|term| args(app, term));
    let before = starts();
    app.open_note_at("deep.md", Some(sidebar::Target::Range(0..1, Some(1))));
    glib::timeout_future(Duration::from_millis(500)).await;
    let front = app.tabs().selected_page().map(|p| p.title().to_string());
    println!(
        "bench editor line hit={hit:?} symbol={symbol:?} again_front={front:?} again_starts={}",
        starts() - before
    );
    // The user's own command line, the fake being its program.
    {
        let mut config = app.config.borrow_mut();
        config.editor = Editor::Custom;
        config.editor_command = "COLORTERM= nano --at {line} {file}".to_string();
    }
    app.config_changed();
    app.open_note_at("custom.md", Some(sidebar::Target::Range(0..1, Some(2))));
    let custom = started(app, "custom.md").await;
    println!(
        "bench editor custom args={:?}",
        custom.as_ref().and_then(|term| args(app, term))
    );
    end_all(app, "line ended", b"0\n").await;
}

async fn panes(app: &Rc<App>) {
    let root = app.root();
    let before = "# Panes\n\n## First\n\nBefore the save.\n";
    std::fs::write(root.join("panes.md"), before).unwrap();
    std::fs::write(root.join("linker.md"), "# Linker\n\nSee [[panes]].\n").unwrap();
    until("editor", "panes reconciled", 30_000, || {
        app.reconciled.get()
    })
    .await;
    pick(app, Editor::Nano);
    app.show_pane("outline");
    app.open_path("panes.md");
    let (Some(term), Some(sidebar)) = (started(app, "panes.md").await, app.sidebar.get()) else {
        return println!("bench editor panes started=false");
    };
    let view_mode = app
        .window
        .lookup_action("view-mode")
        .is_some_and(|action| action.is_enabled());
    app.toggle_preview();
    let shown = || {
        let rendered = app.rendered_of(&term.page)?;
        let preview = rendered.preview();
        let buffer = preview.as_ref()?.buffer();
        Some(
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string(),
        )
    };
    let says = |text: &str| shown().is_some_and(|shown| shown.contains(text));
    let outline = || sidebar.outline_lines(None);
    until("editor", "panes before", 10_000, || {
        says("Before") && outline().len() == 2
    })
    .await;
    let front = app.active_doc().is_some_and(|doc| doc.rendered().is_some());
    println!(
        "bench editor panes view_mode={view_mode} preview_front={front} before={} outline={:?}",
        says("Before"),
        outline()
    );
    // A save, as the editor makes it, while the preview tab is in front.
    let after = format!(
        "{}\n## Saved\n\nAfter the save.\n",
        before.replace("Before", "Still")
    );
    std::fs::write(root.join("panes.md"), after).unwrap();
    let saved = until("editor", "panes saved", 10_000, || {
        says("After") && outline().len() == 3
    })
    .await
    .is_some();
    println!(
        "bench editor panes saved={saved} before={} outline={:?}",
        says("Before"),
        outline()
    );
    shoot(app, "editor-panes-saved.png").await;
    app.reveal_page(&term.page);
    sidebar.show_section("references");
    let links = || sidebar.info_state().0;
    until("editor", "panes references", 10_000, || {
        links().contains("Links:open(1)")
    })
    .await;
    let section = links()
        .split(' ')
        .find(|s| s.starts_with("Links"))
        .map(str::to_string);
    println!("bench editor panes references={section:?}");
    shoot(app, "editor-panes-links.png").await;
    term.view.feed_child(b"0\n");
    let closed = until("editor", "panes closed", 10_000, || {
        app.terminals().is_empty()
    })
    .await
    .is_some();
    println!(
        "bench editor panes closed={closed} previews={}",
        app.rendereds().len()
    );
}

/// Feed every fake still running `answer`, which ends it, and wait for their tabs to hear so.
/// One that has not ten seconds on (`bench editor timeout <what>`) is told apart by
/// [`unended`]: a fake that never got its answer, one started again, or a tab that never heard
/// its fake end.
async fn end_all(app: &Rc<App>, what: &str, answer: &[u8]) {
    for term in app.terminals() {
        term.view.feed_child(answer);
    }
    let ended = || app.terminals().iter().all(|t| t.ended());
    if until("editor", what, 10_000, ended).await.is_none() {
        unended(app);
    }
}

/// What each tab that has not heard its editor end shows last, the pid of every fake started
/// under this `HOME`, the shells the holder still holds, and each process of this launch (its
/// `TMPDIR` in its environment, which the holder, the shells and the fakes inherit), with its
/// state and what it waits in.
fn unended(app: &Rc<App>) {
    for term in app.terminals().iter().filter(|t| !t.ended()) {
        let text = text(term);
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let last = &lines[lines.len().saturating_sub(3)..];
        println!("bench editor unended {} lines={last:?}", term.key());
    }
    // A fake started again after its tab had fed it is one more pid here than tabs were opened.
    let starts = std::fs::read_to_string(glib::home_dir().join("editor-starts"));
    let starts: Vec<String> = starts
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    println!("bench editor unended starts={starts:?}");
    let cli = std::env::current_exe().map(|exe| exe.with_file_name("accent-cli"));
    if let Ok(held) = cli.and_then(|cli| std::process::Command::new(cli).arg("held").output()) {
        for shell in String::from_utf8_lossy(&held.stdout).lines() {
            println!("bench editor unended held {shell}");
        }
    }
    let ours = format!("TMPDIR={}", std::env::var("TMPDIR").unwrap_or_default());
    for proc in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let (pid, proc) = (proc.file_name(), proc.path());
        let environ = std::fs::read(proc.join("environ")).unwrap_or_default();
        if !environ.split(|b| *b == 0).any(|var| var == ours.as_bytes()) {
            continue;
        }
        let read = |name| std::fs::read_to_string(proc.join(name)).unwrap_or_default();
        let stat = read("stat");
        // After the command's name, which may hold spaces: the state, then the parent.
        let mut after = stat
            .rsplit_once(") ")
            .map_or("", |(_, rest)| rest)
            .split(' ');
        let (state, parent) = (after.next().unwrap_or("?"), after.next().unwrap_or("?"));
        println!(
            "bench editor unended process {} parent={parent} state={state} wchan={} {:?}",
            pid.display(),
            read("wchan"),
            read("cmdline").replace('\0', " ").trim_end()
        );
    }
}

/// The window as it is, pictured into `$TMPDIR` as `name`.
async fn shoot(app: &App, name: &str) {
    let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    pumped().await;
    super::diagram::shoot(
        app.window.upcast_ref(),
        &std::path::Path::new(&dir).join(name),
    );
}

/// How many times a fake editor has started under this `HOME`.
fn starts() -> usize {
    let starts = std::fs::read_to_string(glib::home_dir().join("editor-starts"));
    starts.unwrap_or_default().lines().count()
}

/// End every terminal tab unasked and write the session before quitting, so the next step puts
/// back no editor of this one's.
fn finish(app: &Rc<App>) {
    for term in app.terminals() {
        app.forget_page(&term.page);
        app.close_page(&term.page);
    }
    app.save_session();
    bench_quit(app);
}

/// Quit as Ctrl+Q does, through the window's close, which writes the session and lets the editor
/// go on running.
fn quit(app: &Rc<App>) {
    if let Some(gtk_app) = app.window.application() {
        gtk_app.activate_action("quit", None);
    }
}

async fn hold(app: &Rc<App>) {
    // Counted from here: the steps before started editors of their own.
    let _ = std::fs::remove_file(glib::home_dir().join("editor-starts"));
    std::fs::write(app.root().join("held.md"), "# Held\n").unwrap();
    until("editor", "hold reconciled", 30_000, || app.reconciled.get()).await;
    pick(app, Editor::Nano);
    app.open_path("held.md");
    let started = started(app, "held.md").await;
    // Its preview tab, which the session puts back beside it. The editor's own stays in front: a
    // shell put back behind another tab attaches only once it is shown.
    if let Some(term) = &started {
        app.toggle_preview();
        until("editor", "hold preview", 5_000, || {
            app.rendered_of(&term.page).is_some()
        })
        .await;
        app.reveal_page(&term.page);
    }
    println!(
        "bench editor hold started={} starts={}",
        started.is_some(),
        starts()
    );
    quit(app);
}

async fn back(app: &Rc<App>) {
    let term = started(app, "held.md").await;
    let title = term.as_ref().map(|term| term.page.title().to_string());
    let preview = term
        .as_ref()
        .is_some_and(|t| app.rendered_of(&t.page).is_some());
    println!(
        "bench editor back title={title:?} starts={} preview={preview}",
        starts()
    );
    quit(app);
}

/// Every widget under `root` of type `T`, in order.
fn all<T: IsA<gtk::Widget>>(root: &gtk::Widget) -> Vec<T> {
    let mut found: Vec<T> = root.clone().downcast::<T>().into_iter().collect();
    let mut child = root.first_child();
    while let Some(widget) = child {
        found.extend(all::<T>(&widget));
        child = widget.next_sibling();
    }
    found
}

/// The names a combo row lists.
fn names(row: &adw::ComboRow) -> Vec<String> {
    let Some(model) = row.model().and_downcast::<gtk::StringList>() else {
        return Vec::new();
    };
    (0..model.n_items())
        .filter_map(|i| model.string(i))
        .map(|s| s.to_string())
        .collect()
}

async fn prefs(app: &Rc<App>) {
    let _ = WidgetExt::activate_action(&app.window, "win.preferences", None);
    glib::timeout_future(Duration::from_millis(500)).await;
    let Some(dialog) = app.window.visible_dialog() else {
        return println!("bench editor prefs dialog=none");
    };
    let row = all::<adw::ComboRow>(dialog.upcast_ref())
        .into_iter()
        .find(|row| row.title() == "Text Editor");
    let Some(row) = row else {
        return println!("bench editor prefs row=none");
    };
    let listed = |name: &str| names(&row).iter().filter(|n| *n == name).count();
    until("editor", "prefs nano", 10_000, || listed("Nano") > 0).await;
    if let Some(at) = names(&row).iter().position(|n| n == "Nano") {
        row.set_selected(at as u32);
    }
    let chosen = app.config.borrow().editor;
    println!(
        "bench editor prefs nano={} chosen={}",
        listed("Nano"),
        match chosen {
            Editor::Nano => "nano",
            _ => crate::editors::name(chosen),
        }
    );
}
