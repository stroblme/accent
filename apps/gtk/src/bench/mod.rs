//! The `ACCENT_BENCH_*` drills: headless runs under Xvfb that time or probe one interaction,
//! print what they saw to stdout and quit. `install_bench_hooks` says which variable starts which.
//! The drills live in a module per area and share the helpers at the end of this one.

use super::*;

mod chrome;
mod compare;
mod diagram;
mod files;
mod git;
mod image;
mod keys;
mod outline;
mod panes;
mod pdf;
mod replace;
mod search;
mod style;
mod tags;

use chrome::bench_chrome;
use compare::{bench_compare, bench_compare_lines, bench_compare_pads};
use diagram::bench_diagram;
use files::{
    bench_clip, bench_close, bench_expand, bench_hidden, bench_menu, bench_menu_press, bench_paths,
    bench_templates,
};
use git::{bench_git, bench_git_init, bench_git_press};
use image::bench_image;
use keys::{bench_keys, bench_list, bench_shell_keys, bench_term};
use outline::bench_outline;
use panes::{bench_layout, bench_layout_pick, bench_panes, bench_tabs};
use pdf::{bench_drawing, bench_pdf, bench_pdf_stale};
use replace::bench_replace;
use search::bench_search;
use style::{bench_follow, bench_occurrences, bench_reveal, bench_style, bench_theme};
use tags::bench_tags;

/// `ACCENT_BENCH_EXPAND=<rel_path>` and `ACCENT_BENCH_SWITCHER=<query>` time the two interactions
/// that used to stall the main loop, print the numbers to stdout and quit. Both run headless under
/// Xvfb, so "expanding a big directory is still fast" stays a command anyone can re-run rather
/// than a claim in a commit message. `RUST_LOG=accent=debug` adds the per-query breakdown.
/// `ACCENT_BENCH_GIT=1` is the same idea for the Git pane, and prints row counts rather than
/// times, plus the branch readout and how many history rows a background fetch marked as not
/// pulled yet, and then the changes list's splices across a refresh that changes nothing and two
/// Stage clicks. `=press:<path>` instead prints where that row's Stage button is and stays up, for
/// an XTEST press held while the repository changes. `=init` is the pane's own visibility: whether
/// the sidebar has a Git pane either side of a `git init` in the vault root, which it runs itself.
/// `ACCENT_BENCH_KEYS=1` likewise for the editor's key semantics, and prints text and caret
/// positions; `=<rel_note>` instead presses Return and Tab at the end of every list line of that
/// note and prints the ones whose marker or indent did not come out as `typing` says it should,
/// plus the width one indent is worth there, then Tab on lines that already have text on them.
/// It opens with the completion popup: whether "a popup is up" reads true against a real one and
/// false against both a view taken off screen under one and a forged `show`, and that Return
/// still continues a list after them. The popup wants the X input focus, which under Xvfb is
/// `build-aux/xtest.py :<display> "move 700 500; focus"` run beside it.
/// `ACCENT_BENCH_CHROME=1` fires actions at a faded window and prints whether the
/// chrome stayed away; `=<relA>,<relB>` then opens the two notes side by side, prints what each
/// focus level fades, and holds the line fade on screen and times it. `ACCENT_BENCH_PATHS=1`
/// drives a path entry's completion, and prints widths and the text its keys apply.
/// `ACCENT_BENCH_STYLE=<rel_path>` types a heading into a note at two sizes and prints whether it
/// was styled on the keystroke or on the debounce, then whether a copy and paste, a middle click
/// or a drop out of a styled or folded line brings its tags along.
/// `ACCENT_BENCH_PANES=<relA>,<relB>` moves a tab between panes and prints where it landed.
/// `ACCENT_BENCH_COMPARE=<rel_path>` compares a note with its disk copy inside its tab and prints
/// what the panes hold and whether their rows line up. `=pads:<rel_path>` instead stages a note of
/// long paragraphs in a repository it makes itself and types at the start of the two lines whose
/// padding tag does not begin at the newline before them, and `=lines:<rel_path>` stages and
/// unstages one line of a note it commits in a repository of its own.
/// `ACCENT_BENCH_IMAGE=<rel_png>,<rel_other_png>` zooms an image and replaces its file with one of
/// another size, printing what the picture asks for and says either side of the reload.
/// `ACCENT_BENCH_TERM=1` prints what a shell window calls itself — the window title, the header's
/// two lines and the tab's — until VTE has reported a title of its own. Against `--terminal` that
/// is the vault-less window; against a vault it opens a shell in a tab and covers that instead.
/// `ACCENT_BENCH_SHELL_KEYS=1` focuses a shell in a window that does not have the keyboard and
/// prints what `Ctrl+S` activates.
/// `ACCENT_BENCH_PDF=<rel_path>` opens a PDF, fits it to the page from a mid-page scroll position
/// and prints the layout either side of it, then appends a page with `win.pdf-add-page` and
/// prints the page count, where the reader landed and the page sizes the file holds on disk once
/// the save has run, and what the vault itself then holds — on a remote vault the host's own copy,
/// which is the only witness that the write was uploaded. It then renames the file the way a
/// dropped row does and appends another page to it, which is the render thread following the new
/// name. It writes to the document and moves it, so point it at a scratch copy; and point it at a
/// document of several pages, since a one-page PDF is wholly on screen whatever the scroll offset
/// was. `=stale:<rel_path>` is the remote vault's etag gate: it stamps the cached copy with an
/// etag the host never had, appends a page and prints whether the host's copy is untouched and
/// what `<name> (drawn).pdf` beside it holds, then appends another and prints the same again —
/// the second refusal must write that same copy rather than a numbered one, and must leave the
/// toast count where the first put it.
/// `ACCENT_BENCH_DRAWING=1` fires New Drawing at the vault root, prints what the dialog came up
/// with, answers it with the window-shaped size and prints the file that landed and the tool the
/// tab it opened has in hand.
/// `ACCENT_BENCH_TABS=<rel_note>,<rel_pdf>` walks a note, a shell and a PDF through one pane and
/// closes the lot, printing what the find bar and the Outline pane say at each step: what a tab
/// switch and the last tab's close leave behind. The note opens as a preview and is kept by its
/// eye first, and its title and indicator are printed either side of that.
/// `ACCENT_BENCH_FOLLOW=<rel_note>` puts the pointer on a wikilink and on a plain word with Ctrl
/// held, and prints what the Ctrl+hover underline covers; then it follows a link nothing answers
/// to from the caret, as F12 does, and prints the dialog that offers to create it, with the name
/// it arrives prefilled with.
///
/// `ACCENT_BENCH_OUTLINE=<rel_note>,<rel_other>` walks the caret down a note and prints which
/// Outline row is selected, whether it is in view and who has the keyboard; then again after a
/// switch to `<rel_other>` and back, and after the caret moved while the pane was hidden.
/// `=hold:<rel_note>` prints where to aim and then the pane's state as XTEST drives it, for 20 s.
///
/// `ACCENT_BENCH_OCCUR=<rel_note>` selects things in a note and prints what the muted occurrence
/// highlight made of each selection, plus the two match colours and the priorities of the tags
/// they are painted with.
///
/// `ACCENT_BENCH_THEME=<rel_note>` walks the window through Light, Dark and Solarized the way a
/// system switch and the preferences do, and prints what the note's theme-derived tags hold on
/// each side of every switch: as it lands, and again once the restyle it defers has run. A second
/// launch with `ADW_DEBUG_COLOR_SCHEME=prefer-dark` is Solarized's other half.
///
/// `ACCENT_BENCH_REVEAL=<rel_note>` jumps into a note as a search hit, a tag and a Go to Line
/// each do, and prints what the temporary reveal painted and what takes it down again, plus the
/// three match tags' priorities.
///
/// `ACCENT_BENCH_CLOSE=1` opens a note, cancels a New File and an Unsaved Changes dialog, quits,
/// and prints how many references to the vault the closed window left behind and whether either
/// dialog outlived it; it exits 1 unless nothing did.
///
/// `ACCENT_BENCH_CLIP=<rel_file>` copies a file and pastes it beside itself, then cuts the copy
/// and pastes it in the vault root: the `(copy)` mark, the rows a Cut dims, and whether the paste
/// of a Cut moved the file rather than copying it again. Last it puts two files on the clipboard
/// at once, as a Ctrl+click set does, and prints whether both landed in the vault root.
/// `ACCENT_BENCH_MENU=<rel_file>` opens a tree row's context menu and takes the pointer off the
/// list the way the popover's own grab does, printing which row stays highlighted while the menu
/// is up and which once it has closed. Then it marks that row and one more, the way a Ctrl+click
/// does, and prints the set, the items a menu over one of them offers, the rows drawn with the
/// mark on them, and the same once the marks are let go. `=press:<rel_file>` instead reveals that
/// row, prints where it is on screen and stays up for an XTEST Ctrl+click — `build-aux/xtest.py
/// :99 "move X Y; keydown ctrl; down; up; keyup ctrl"` — printing the marked rows and how many
/// documents are open every five seconds, which is how the modifier half is driven at all.
///
/// `ACCENT_BENCH_TAGS=<rel_note>` writes a marker tag into a note and takes it away again with
/// the Tags pane on screen, printing whether the pane's list holds the marker at each step.
///
/// `ACCENT_BENCH_REPLACE=1` writes a note holding one unique word, presses the Search pane's
/// Replace All on it and prints what the pane lists before and after the rewrite.
///
/// `ACCENT_BENCH_SEARCH=<query>[:<n>]` leaves `<query>` in the Search pane and writes `n` notes
/// holding it behind the pane's back, printing the rows before, after and once they are gone
/// again — once ranked, then once more with the replace row open, which is the exact scan, and
/// last with another pane in front, which is the catch-up the Search pane owes on its way back.
///
/// `ACCENT_BENCH_HIDDEN=1` prints the Files pane's rows and which of them are dimmed, then toggles
/// Show Hidden Files off and on again, printing them after each.
///
/// `ACCENT_BENCH_DIAGRAM=<rel>` edits a diagram (a sample is written there if there is none) and
/// prints each step through the save; `=shot:<rel>:<dir>` paints every page into `<dir>`.
///
/// `ACCENT_BENCH_LAYOUT=<a>,<b>,<c>,<d>` lays four notes out as `[a b | [c / d]]`, `a` in front
/// on the left and `c`'s pane active, with the handles at 30 % and 60 %, prints the tree and
/// quits the way Ctrl+Q does, which writes the session; a fifth field of `shell` goes back to
/// `a`'s pane and puts a terminal in front of it first, a shell being no file and so not
/// restored, which leaves that pane naming none. Every printout says
/// which tab is in front of the active pane, which tab the session would write as the active one,
/// and how many places each pane's Back and Forward hold. `=1` prints the tree a restore built
/// once its tabs have landed, and quits without writing one. `=pick:<rel>` does the same, having
/// selected `<rel>` in its pane as a click on its tab would, between two tabs landing;
/// `=focus:<rel>` gives it the keyboard instead, and `=open:<rel>` opens a note the session does
/// not hold before any tab has landed, as a reader would into the still-empty active pane. On a
/// remote vault they wait for the host to
/// answer, `<a>,…` and `=1` printing what the window shows until then, and `=quit` quits there the
/// way Ctrl+Q does.
pub fn install_bench_hooks(app: &Rc<App>) {
    let expand = std::env::var("ACCENT_BENCH_EXPAND").ok();
    let switcher = std::env::var("ACCENT_BENCH_SWITCHER").ok();
    let style = std::env::var("ACCENT_BENCH_STYLE").ok();
    let git = std::env::var("ACCENT_BENCH_GIT").ok();
    let keys = std::env::var("ACCENT_BENCH_KEYS").ok();
    let chrome = std::env::var("ACCENT_BENCH_CHROME").ok();
    let templates = std::env::var("ACCENT_BENCH_TEMPLATE").is_ok();
    let paths = std::env::var("ACCENT_BENCH_PATHS").is_ok();
    let panes = std::env::var("ACCENT_BENCH_PANES").ok();
    let shell_keys = std::env::var("ACCENT_BENCH_SHELL_KEYS").is_ok();
    let term = std::env::var("ACCENT_BENCH_TERM").is_ok();
    let picture = std::env::var("ACCENT_BENCH_IMAGE").ok();
    let compare = std::env::var("ACCENT_BENCH_COMPARE").ok();
    let pdf = std::env::var("ACCENT_BENCH_PDF").ok();
    let drawing = std::env::var("ACCENT_BENCH_DRAWING").is_ok();
    let tabs = std::env::var("ACCENT_BENCH_TABS").ok();
    let occur = std::env::var("ACCENT_BENCH_OCCUR").ok();
    let theme = std::env::var("ACCENT_BENCH_THEME").ok();
    let reveal = std::env::var("ACCENT_BENCH_REVEAL").ok();
    let follow = std::env::var("ACCENT_BENCH_FOLLOW").ok();
    let outline = std::env::var("ACCENT_BENCH_OUTLINE").ok();
    let close = std::env::var("ACCENT_BENCH_CLOSE").is_ok();
    let hidden = std::env::var("ACCENT_BENCH_HIDDEN").is_ok();
    let layout = std::env::var("ACCENT_BENCH_LAYOUT").ok();
    let diagram = std::env::var("ACCENT_BENCH_DIAGRAM").ok();
    let clip = std::env::var("ACCENT_BENCH_CLIP").ok();
    let menu = std::env::var("ACCENT_BENCH_MENU").ok();
    let tags = std::env::var("ACCENT_BENCH_TAGS").ok();
    let replace = std::env::var("ACCENT_BENCH_REPLACE").is_ok();
    let search = std::env::var("ACCENT_BENCH_SEARCH").ok();
    if expand.is_none()
        && tags.is_none()
        && !replace
        && search.is_none()
        && clip.is_none()
        && menu.is_none()
        && diagram.is_none()
        && switcher.is_none()
        && style.is_none()
        && panes.is_none()
        && compare.is_none()
        && pdf.is_none()
        && !drawing
        && tabs.is_none()
        && occur.is_none()
        && theme.is_none()
        && reveal.is_none()
        && follow.is_none()
        && outline.is_none()
        && layout.is_none()
        && git.is_none()
        && keys.is_none()
        && chrome.is_none()
        && !templates
        && !paths
        && !shell_keys
        && !term
        && picture.is_none()
        && !close
        && !hidden
    {
        return;
    }
    // Not after the first frame: a restore has landed every tab by then.
    if let Some(arg) = layout.as_deref().filter(|arg| arg.contains(':')) {
        return bench_layout_pick(app, arg);
    }
    let app = app.clone();
    // After the first frame, so widget realisation is not counted in the numbers.
    glib::timeout_add_local_once(Duration::from_millis(400), move || {
        if let Some(rels) = panes {
            return bench_panes(&app, &rels);
        }
        if let Some(arg) = layout {
            return bench_layout(&app, &arg);
        }
        if let Some(rel) = compare {
            if let Some(rel) = rel.strip_prefix("lines:") {
                return bench_compare_lines(&app, rel);
            }
            return match rel.strip_prefix("pads:") {
                Some(rel) => bench_compare_pads(&app, rel),
                None => bench_compare(&app, &rel),
            };
        }
        if let Some(rel) = pdf {
            return match rel.strip_prefix("stale:") {
                Some(rel) => bench_pdf_stale(&app, rel),
                None => bench_pdf(&app, &rel),
            };
        }
        if drawing {
            return bench_drawing(&app);
        }
        if let Some(rel) = clip {
            return bench_clip(&app, &rel);
        }
        if let Some(rel) = tags {
            return bench_tags(&app, &rel);
        }
        if replace {
            return bench_replace(&app);
        }
        if let Some(query) = search {
            return bench_search(&app, &query);
        }
        if let Some(rel) = menu {
            if let Some(rel) = rel.strip_prefix("press:") {
                return bench_menu_press(&app, rel);
            }
            return bench_menu(&app, &rel);
        }
        if let Some(arg) = diagram {
            return bench_diagram(&app, &arg);
        }
        if let Some(rels) = tabs {
            return bench_tabs(&app, &rels);
        }
        if let Some(rel) = occur {
            return bench_occurrences(&app, &rel);
        }
        if let Some(rel) = theme {
            return bench_theme(&app, &rel);
        }
        if let Some(rel) = reveal {
            return bench_reveal(&app, &rel);
        }
        if let Some(rel) = follow {
            return bench_follow(&app, &rel);
        }
        if let Some(arg) = outline {
            return bench_outline(&app, &arg);
        }
        if shell_keys {
            return bench_shell_keys(&app);
        }
        if term {
            return bench_term(&app);
        }
        if let Some(arg) = picture {
            return bench_image(&app, &arg);
        }
        if close {
            return bench_close(&app);
        }
        if hidden {
            return bench_hidden(&app);
        }
        if paths {
            return bench_paths(&app);
        }
        if templates {
            return bench_templates(&app);
        }
        if let Some(notes) = chrome {
            return bench_chrome(&app, &notes);
        }
        if let Some(arg) = keys {
            return match arg.as_str() {
                "1" => bench_keys(&app),
                rel => bench_list(&app, rel),
            };
        }
        if let Some(arg) = git {
            return match (arg.strip_prefix("press:"), arg.as_str()) {
                (Some(path), _) => bench_git_press(&app, path),
                (None, "init") => bench_git_init(&app),
                _ => bench_git(&app),
            };
        }
        if let Some(rel) = style {
            return bench_style(&app, &rel);
        }
        if let Some(rel) = expand {
            bench_expand(&app, &rel);
        }
        let Some(query) = switcher else {
            bench_quit(&app);
            return;
        };
        let t0 = Instant::now();
        let _ = WidgetExt::activate_action(&app.window, "win.palette-files", None);
        println!("bench switcher_open_ms {:.1}", ms_since(t0));

        // A query of "1" just means "open it"; anything else is typed into the entry so the
        // debounce, the lazy corpus load and the match all get exercised.
        let entry = (query != "1")
            .then(|| {
                app.window
                    .visible_dialog()
                    .and_then(|d| find_search_entry(d.upcast_ref()))
            })
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

/// First `GtkSearchEntry` in `w`'s subtree, which the bench drives directly because the headless
/// image has no xdotool.
///
/// The caller must pass the palette dialog, not the window: a window holds the sidebar's search
/// entry too, and it comes first in tree order, so searching from the window typed the benchmark's
/// query into the sidebar and measured nothing.
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

/// What a toast standing over the window reads, which is how a drill sees one: libadwaita gives
/// no way to ask the overlay what it is showing.
fn bench_said(app: &Rc<App>) -> Option<String> {
    let label = find_widget(app.window.upcast_ref(), &|w| {
        w.downcast_ref::<gtk::Label>()
            .is_some_and(|l| l.label().starts_with("Cannot "))
    })?;
    Some(label.downcast::<gtk::Label>().ok()?.label().to_string())
}

/// The first widget in `root`'s subtree, `root` included, that `found` accepts.
fn find_widget(root: &gtk::Widget, found: &dyn Fn(&gtk::Widget) -> bool) -> Option<gtk::Widget> {
    if found(root) {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(c) = child {
        if let Some(hit) = find_widget(&c, found) {
            return Some(hit);
        }
        child = c.next_sibling();
    }
    None
}

/// Emit a key press on the entry's own key controller: the headless image has no window manager
/// to give the toplevel the keyboard, and no xdotool to press anything with.
fn press_key(entry: &gtk::Entry, key: gdk::Key) {
    use glib::translate::IntoGlib;
    let controllers = entry.observe_controllers();
    for i in 0..controllers.n_items() {
        let Some(keys) = controllers
            .item(i)
            .and_downcast::<gtk::EventControllerKey>()
        else {
            continue;
        };
        keys.emit_by_name::<bool>(
            "key-pressed",
            &[&key.into_glib(), &0u32, &gdk::ModifierType::empty()],
        );
    }
}

/// Turn the main loop until it has nothing left to dispatch. The find bar's own highlight is
/// scanned on an idle, so its tag is on nothing at all the instant its query is set.
fn bench_pump() {
    let context = glib::MainContext::default();
    for _ in 0..10_000 {
        if !context.iteration(false) {
            return;
        }
    }
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
