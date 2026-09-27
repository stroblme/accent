//! The `ACCENT_BENCH_*` drills: headless runs under Xvfb that time or probe one interaction,
//! print what they saw to stdout and quit. `install_bench_hooks` says which variable starts which.
//! The drills live in a module per area and share the helpers at the end of this one.

use super::*;

mod attach;
mod chrome;
mod compare;
mod diagnostics;
mod diagram;
mod files;
mod find;
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

use attach::bench_attach;
use chrome::{bench_chrome, bench_chrome_keys};
use compare::{
    bench_compare, bench_compare_conflict, bench_compare_diag, bench_compare_gutter,
    bench_compare_lines, bench_compare_pads, bench_compare_row,
};
use diagnostics::bench_diagnostics;
use diagram::bench_diagram;
use files::{
    bench_clip, bench_clip_outside, bench_close, bench_drop, bench_expand, bench_hidden,
    bench_menu, bench_menu_press, bench_paths, bench_save_as, bench_templates, bench_transfer,
    bench_watch,
};
use find::bench_find;
use git::{
    bench_git, bench_git_close, bench_git_focus, bench_git_init, bench_git_markers,
    bench_git_press, bench_git_rebase, bench_git_switch, bench_git_sync_over_fetch,
};
use image::{bench_image, bench_image_look, bench_preview_look};
use keys::{
    bench_box_drag, bench_hold, bench_keys, bench_list, bench_occurrence_keys, bench_shell_keys,
    bench_term,
};
use outline::bench_outline;
use panes::{
    bench_collapse, bench_layout, bench_layout_pick, bench_panes, bench_pin, bench_pin_window,
    bench_pins_restored, bench_tabs,
};
use pdf::{
    bench_drawing, bench_pdf, bench_pdf_bookmarks, bench_pdf_pages, bench_pdf_stale,
    bench_pdf_strip,
};
use replace::bench_replace;
use search::bench_search;
use style::{
    bench_drag_fold, bench_follow, bench_numbers, bench_occurrences, bench_reveal, bench_seam,
    bench_style, bench_theme, bench_wrap,
};
use tags::bench_tags;

/// `ACCENT_BENCH_EXPAND=<rel_path>` and `ACCENT_BENCH_SWITCHER=<query>` time the two interactions
/// that used to stall the main loop, print the numbers to stdout and quit, the switcher with the
/// first rows its query shows. Both run headless under
/// Xvfb, so "expanding a big directory is still fast" stays a command anyone can re-run rather
/// than a claim in a commit message. `RUST_LOG=accent=debug` adds the per-query breakdown.
/// `ACCENT_BENCH_GIT=1` is the same idea for the Git pane, and prints row counts rather than
/// times, plus the branch readout and how many history rows a background fetch marked as not
/// pulled yet, then what a commit row's two buttons are and whether the revealer holds them away
/// until the pointer is on the row, and then the changes list's splices across a refresh that
/// changes nothing and two Stage clicks, and each section header's buttons and whether its row
/// takes the hover highlight; last the Changes header's Discard All, its question answered.
/// `=press:<path>` instead prints where that row's Stage button is and stays up, for an XTEST
/// press held while the repository changes. `=init` is the pane's own visibility: whether
/// the sidebar has a Git pane either side of a `git init` in the vault root, which it runs itself.
/// `=close:<pull|push|fetch>` closes the window while git runs there and prints what the close did,
/// and `=sync` asks for a Sync during the fetch on opening and prints whether it waited for it.
/// `=focus` clicks rows and walks the keyboard over them through XTEST, and prints whether each
/// row's buttons are out. `=switch` picks the second repository and clicks the history's first row
/// at once, and prints what that asked for. `=markers:<rel>` resolves the conflict blocks a merge
/// left in a note through their buttons and the palette (see `git::bench_git_markers`).
/// `ACCENT_BENCH_KEYS=1` likewise for the editor's key semantics, and prints text and caret
/// positions; `=<rel_note>` instead presses Return and Tab at the end of every list line of that
/// note and prints the ones whose marker or indent did not come out as `typing` says it should,
/// plus the width one indent is worth there, then Tab on lines that already have text on them.
/// `=occur:<rel>` asks for XTEST presses of `Alt+J`, `Ctrl+Shift+L` and `Shift+Alt+Up` / `Down`
/// with typing after them, and prints every selection and the buffer after each (see
/// `keys::bench_occurrence_keys`). `=box:<rel>` asks for XTEST drags and a press with `Shift+Alt`
/// and without, and prints the carets and the selection after each (see `keys::bench_box_drag`).
/// It opens with the completion popup: whether "a popup is up" reads true against a real one,
/// that Return at the end of a list item under it continues the list while no row is selected and
/// is the popup's once one is, that "up" reads false against both a view taken off screen under
/// one and a forged `show`, and that Return still continues a list after them. The popup wants the X input focus, which under Xvfb is
/// `build-aux/xtest.py :<display> "move 700 500; focus"` run beside it.
/// `ACCENT_BENCH_CHROME=1` fires actions at a faded window and prints whether the
/// chrome stayed away; `=<relA>,<relB>` then opens the two notes side by side, prints what each
/// focus level fades, and holds the line fade on screen and times it. `=keys:<note>,<pdf>` asks
/// for XTEST presses of the keys that step through a note, the preview and a PDF, and prints
/// whether each one faded the chrome (see `chrome::bench_chrome_keys`). `ACCENT_BENCH_PATHS=1`
/// drives a path entry's completion, and prints widths and the text its keys apply.
/// `ACCENT_BENCH_STYLE=<rel_path>` types a heading into a note at two sizes and prints whether it
/// was styled on the keystroke or on the debounce, then whether a copy and paste, a middle click
/// or a drop out of a styled or folded line brings its tags along. `=wrap:<rel>,<rel>…` opens each
/// file in a narrow window and prints where every line's wrapped rows hang, then times the wrap
/// indent on 10k lines of code in the last one. `=dragfold:<rel>` selects a folded section and
/// prints where to press and let go for XTEST, then what a real drag of it left in the note.
/// `=seam:<rel>` joins a line to a fold with Delete and with Backspace, asks for the iter at every
/// pixel row, and prints what stays hidden: a line left partly hidden aborts it inside GTK.
/// `ACCENT_BENCH_PANES=<relA>,<relB>` moves a tab between panes and prints where it landed, then
/// steps the split it leaves with Move Divider from a dragged 47 % and prints the share each time.
/// `ACCENT_BENCH_COMPARE=<rel_path>` compares a note with its disk copy inside its tab and prints
/// what the panes hold and whether their rows line up. `=pads:<rel_path>` instead stages a note of
/// long paragraphs in a repository it makes itself and types at the start of the two lines whose
/// padding tag does not begin at the newline before them, and `=lines:<rel_path>` stages and
/// unstages one line of a note it commits in a repository of its own. `=row:<repo_rel>` activates
/// that file's Changes row, as a click on it does, and prints what the comparison it opened holds;
/// `=row:stale:<repo_rel>` stages the file behind the pane's back first, so the row it activates
/// is one git has outgrown and the comparison would have nothing to show. `=row:staged:<repo_rel>`
/// and `=row:commit:<repo_rel>` are the same shape for the two that open a tab of their own: a
/// Staged row unstaged behind the pane's back, and the file at HEAD against HEAD~1 where HEAD did
/// not touch it; where HEAD did, the tab it opens says whether its first change is on screen and
/// where the shared scrollbar is. `=diag:<rel_text_file>`
/// collapses a run with warnings in it and prints how many end-of-line messages and gutter marks
/// each state drew: the messages of a hidden run go, the icons stay. It then folds a block over
/// the same file, which hides lines the same way, and reads the two numbers again without
/// publishing anything: a fold's header keeps its own message, the lines under it do not.
/// `=gutter:<rel_text_file>` puts a warning and a fold chevron on a line padded below and on one
/// padded above, prints each one's cell and first row, and holds the window up for a screenshot.
/// `=conflict:<rel_text_file>` writes a sync conflict copy beside that file while its tab is open
/// and prints what the banner stands for: live, after the tab is opened again, and once the copy
/// is gone.
/// `ACCENT_BENCH_IMAGE=<rel_png>,<rel_other_png>` zooms an image and replaces its file with one of
/// another size, printing what the picture asks for and says either side of the reload.
/// `ACCENT_BENCH_IMAGE_LOOK=<rel>,<rel>,…` walks each image through the three themes, inverted and
/// not, printing what the classifier said, the pixel the tab shows at (2,2) and the recolouring's
/// cost (see `image::bench_image_look`). `ACCENT_BENCH_PREVIEW_LOOK=<rel_note>` does the same for
/// the images a note shows in the preview, with the requests the page made (`bench_preview_look`).
/// `ACCENT_BENCH_TERM=1` prints what a shell window calls itself — the window title, the header's
/// two lines and the tab's — until VTE has reported a title of its own. Against `--terminal` that
/// is the vault-less window; against a vault it opens a shell in a tab and covers that instead.
/// The vault-less window then answers Save Session with a name and prints whether its primary
/// menu offers Close Session either side, and what it is asked before replacing the session a
/// run before it saved under that name; give it a scratch `TMPDIR`, or its shell joins the
/// holder of whatever session of accent is running.
/// `ACCENT_BENCH_SHELL_KEYS=1` focuses a shell in a window that does not have the keyboard and
/// prints what `Ctrl+S` activates.
/// `ACCENT_BENCH_HOLD=open` then `=back`, on one scratch state, is a shell outliving its window:
/// `open` leaves a marker in it and quits, `back` prints what the restore brought back and whether
/// Close Tab ended it in the holder (see `keys::bench_hold`). `=early` closes new shells before
/// they can have started and prints whether any is held for nobody.
/// `ACCENT_BENCH_PDF=<rel_path>` opens a PDF, fits it to the page from a mid-page scroll position
/// and prints the layout either side of it, then adds a page with `win.pdf-add-page-after` and
/// prints the page count, where the reader landed and the page sizes the file holds on disk once
/// the save has run, and what the vault itself then holds — on a remote vault the host's own copy,
/// which is the only witness that the write was uploaded. It then renames the file the way a
/// dropped row does and adds another page to it, which is the render thread following the new
/// name. It writes to the document and moves it, so point it at a scratch copy; and point it at a
/// document of several pages, since a one-page PDF is wholly on screen whatever the scroll offset
/// was. `=stale:<rel_path>` is the remote vault's etag gate: it stamps the cached copy with an
/// etag the host never had, adds a page and prints whether the host's copy is untouched and
/// what `<name> (edited).pdf` beside it holds, then adds another and prints the same again —
/// the second refusal must write that same copy rather than a numbered one, and must leave the
/// toast count where the first put it. `=pages:<rel_path>` moves the first page below the third as
/// a drop in the thumbnail strip does, adds a page before the one being read and one after the last
/// page, and deletes the first through the window actions, which asks nothing, then walks
/// all four back with Undo and forward again with Redo, and prints the page being read and each
/// page's text on disk after every step, with the pages a note written first links to (a
/// highlight, a jump and a markdown link into pages 1 to 3) and what the toast said; then the
/// items of the page's own menu without and with a selection, and of the status bar's page
/// count's menu.
/// Point it at a scratch copy of the generated vault's `Attachments/pages.pdf`.
/// `=strip:<rel_path>` is the pointer's half, held for XTEST: it opens the document with the
/// Outline pane up and prints the same every two seconds for 40 s, so a hover and a drag along the
/// thumbnail strip (`build-aux/xtest.py :N "drag X0 Y0 X1 Y1"`) can be watched landing in the
/// file. `=bookmarks:<rel_path>[,<rel_diagram>]` scrolls through the document with the Outline pane
/// up and prints the bookmark each page is under, whether it is in view and who has the keyboard,
/// then the same after a page edit and whether the list is the one it was; with a diagram, the
/// same for each of its pages (`pdf::bench_pdf_bookmarks`).
/// `ACCENT_BENCH_DRAWING=1` fires New Drawing at the vault root, prints what the dialog came up
/// with, answers it with the window-shaped size and prints the file that landed and the tool the
/// tab it opened has in hand.
/// `ACCENT_BENCH_TABS=<rel_note>,<rel_pdf>` walks a note, a shell and a PDF through one pane and
/// closes the lot, printing what the find bar and the Outline pane say at each step: what a tab
/// switch and the last tab's close leave behind. The note opens as a preview and is kept by its
/// eye first, and its title and indicator are printed either side of that.
/// `=pin:<a>,<b>,<c>,<d>` opens four notes in one pane and prints each pane's tabs, pinned ones
/// marked `^`, after every step: `c` pinned from its tab menu and `b` from the palette, `c`
/// unpinned and pinned again, `a` and then `b` moved across the pinned ones as a drag along the
/// bar ends, and `d` and then `c` moved right with Move Tab. It quits the way Ctrl+Q does, which
/// writes the session; `=pins` on the same scratch state prints what the restore brought back.
/// `=pinwin:<a>,<b>,<c>` pins `a` among three notes, then hands `b` and then `a` to the window
/// kept for loose files the way a drop there does, and prints both windows' tabs after each. On a
/// remote vault it waits for the host, and neither tab may leave: the file is on the host.
/// `ACCENT_BENCH_FOLLOW=<rel_note>` puts the pointer on a wikilink, on a plain word and on a bare
/// URL with Ctrl held, and prints what the Ctrl+hover underline covers and the URL under the caret;
/// then it follows a link nothing answers to from the caret, as F12 does, and prints the dialog
/// that offers to create it, with the name it arrives prefilled with. Then `[[#Nowhere]]`, which
/// must put the caret on the note's first line and toast, as must a preview click on
/// `[[#Elsewhere]]`; `[[#^blk]]` followed both ways, which must land on the block it marks; and a
/// link typed at the end of a note past 16 K characters and followed at once, which must offer New
/// File as well. Any text file will do for the underline: a `.txt` has no wikilinks, and its URL
/// underlines all the same.
///
/// `ACCENT_BENCH_OUTLINE=<rel_note>,<rel_other>` walks the caret down a note and prints which
/// Outline row is selected, whether it is in view and who has the keyboard; then again after a
/// switch to `<rel_other>` and back, and after the caret moved while the pane was hidden; last
/// the list's scroll and row through a few edits, which leave them alone, and one caret move.
/// `=hold:<rel_note>` prints where to aim and then the pane's state as XTEST drives it, for 20 s.
///
/// `ACCENT_BENCH_OCCUR=<rel_note>` selects things in a note and prints what the muted occurrence
/// highlight made of each selection, plus the two match colours and the priorities of the tags
/// they are painted with.
///
/// `ACCENT_BENCH_NUMBERS=<rel_note>,<rel_code>,<rel_note>` opens the first two and flips the Line
/// Numbers switch in the preferences three times, printing after each step whether every open tab
/// shows its numbers, how wide the column is and whether the tab is in front: the third file
/// opens after the first flip, and the last flip is made with both notes behind the code. Then
/// `config.toml` turns them off by hand with the dialog up, and it prints what the dialog's row
/// says afterwards, whether accent wrote the file again, and whether that row still writes it.
///
/// `ACCENT_BENCH_THEME=<rel_note>` walks the window through Light, Dark and Solarized the way a
/// system switch and the preferences do, and prints what the note's theme-derived tags and the
/// hues a CSV's columns and the git lanes share hold on each side of every switch: as it lands, and again once the restyle it defers has run. A second
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
/// of a Cut moved the file rather than copying it again. Then it puts two files on the clipboard
/// at once, as a Ctrl+click set does, and prints whether both landed in the vault root, how many
/// toasts said so — one is right — and what it said. Last it
/// cuts two notes that link each other and a third, pastes them into the file's folder and
/// answers the Update Links? question, printing every dialog that came — one is right — and
/// the three notes' texts afterwards. Each paste is waited for, up to a minute, not timed.
/// `ACCENT_BENCH_CLIP=outside:<dir>` pastes a folder and a file made on this machine into `<dir>`
/// ("" is the root) as GNOME Files would, and prints what landed and the toast
/// (`files::bench_clip_outside`).
/// `ACCENT_BENCH_TRANSFER=<rel_pdf>` opens, downloads and uploads over a remote vault and prints
/// the status bar's byte counts for each (`files::bench_transfer`).
/// `ACCENT_BENCH_ATTACH=<rel_note>,<rel_vault_png>,<rel_code>` pastes and drops images into a
/// note and prints the text and the files they left (`attach::bench_attach`).
/// `ACCENT_BENCH_SAVE_AS=<rel_file>` saves the file — a note, a diagram, a PDF or an image — as
/// another in a folder not there yet, and a note also onto a folder, onto a file open in another
/// tab and as `.txt` (`bench_save_as`).
/// `ACCENT_BENCH_MENU=<rel_file>` opens a tree row's context menu and takes the pointer off the
/// list the way the popover's own grab does, printing which row stays highlighted while the menu
/// is up and which once it has closed. Then it marks that row and one more, the way a Ctrl+click
/// does, and prints the set, the items a menu over one of them offers, the rows drawn with the
/// mark on them, and the same once the marks are let go. Then a Shift+click's range from that
/// row to the first shut folder below it: the set, the folder opened with its rows drawn marked,
/// and a Ctrl+click on one of them taking that one alone out. `=press:<rel_file>` instead
/// reveals that row and prints where it and the row two below it are on screen, and stays up
/// for an XTEST Ctrl+click or Shift+click — `build-aux/xtest.py :99 "move X Y; keydown ctrl;
/// down; up; keyup ctrl"` — printing the marked rows and how many documents are open every five
/// seconds, which is how the modifier half is driven at all.
///
/// `ACCENT_BENCH_TAGS=<rel_note>` writes a marker tag into a note and takes it away again with
/// the Tags pane on screen, printing whether the pane's list holds the marker at each step.
///
/// `ACCENT_BENCH_REPLACE=1` writes a note holding one unique word, presses the Search pane's
/// Replace All on it and prints what the pane lists before and after the rewrite.
///
/// `ACCENT_BENCH_FIND=<rel_note>` uses two queries in a note's find bar and then presses Ctrl+F
/// twice over the open bar — once on a typed query, once on one Up recalled — printing what is
/// selected in the box each time and again once its delayed search has run.
///
/// `ACCENT_BENCH_DIAG=<rel_code_file>` hands a code tab an error, a warning and a hint, then
/// presses the status bar's count twice, printing what the count says and how much of the answer
/// the text is carrying each time.
///
/// `ACCENT_BENCH_SEARCH=<query>[:<n>]` leaves `<query>` in the Search pane and writes `n` notes
/// holding it behind the pane's back, printing the rows before, after and once they are gone
/// again — once ranked, then once more with the replace row open, which is the exact scan, and
/// last with another pane in front, which is the catch-up the Search pane owes on its way back.
/// `=type:<query>` types a ranked query and prints each change of the rows with the milliseconds
/// since the last keystroke: the prefix rows, then the mid-word rows appended below them;
/// `=walk:<query>` does the same with All on, the walk past the index appending its rows last,
/// under Not Indexed. `=more:<query>[:<n>]` opens a note's "+N more in this file" row, printing
/// the rows, the count, the tabs and the scroll before and after, and again after the same query
/// is asked again and a new one; `=click:` waits for a real XTEST press on it instead.
///
/// `ACCENT_BENCH_HIDDEN=1` prints the Files pane's rows and which of them are dimmed, then toggles
/// Show Hidden Files off and on again, printing them after each.
///
/// `ACCENT_BENCH_DIAGRAM=<rel>` edits a diagram (a sample is written there if there is none) and
/// prints each step through the save; `=shot:<rel>:<dir>` paints every page into `<dir>`.
///
/// `ACCENT_BENCH_COLLAPSE=1` drags the sidebar to 400 px and takes the window to 360 px, below the
/// width the sidebar collapses at, and back, printing the window's width, whether the sidebar
/// shows, where the divider is and what a session would save (sidebar, width): before, narrow,
/// after F9 twice while narrow, wide again, and the same round with the sidebar hidden before it.
/// The divider has to read 400 again once the window is wide.
///
/// `ACCENT_BENCH_LAYOUT=<a>,<b>,<c>,<d>` lays four notes out as `[a b | [c / d]]`, `a` in front
/// on the left and `c`'s pane active, with the handles at 30 % and 60 %, prints the tree and
/// quits the way Ctrl+Q does, which writes the session; a fifth field of `shell` goes back to
/// `a`'s pane and puts a terminal in front of it first, which the session writes into that pane
/// and as the active tab like any other. Every printout says
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
    let hold = std::env::var("ACCENT_BENCH_HOLD").ok();
    let picture = std::env::var("ACCENT_BENCH_IMAGE").ok();
    let image_look = std::env::var("ACCENT_BENCH_IMAGE_LOOK").ok();
    let preview_look = std::env::var("ACCENT_BENCH_PREVIEW_LOOK").ok();
    let compare = std::env::var("ACCENT_BENCH_COMPARE").ok();
    let pdf = std::env::var("ACCENT_BENCH_PDF").ok();
    let drawing = std::env::var("ACCENT_BENCH_DRAWING").is_ok();
    let tabs = std::env::var("ACCENT_BENCH_TABS").ok();
    let occur = std::env::var("ACCENT_BENCH_OCCUR").ok();
    let theme = std::env::var("ACCENT_BENCH_THEME").ok();
    let numbers = std::env::var("ACCENT_BENCH_NUMBERS").ok();
    let reveal = std::env::var("ACCENT_BENCH_REVEAL").ok();
    let follow = std::env::var("ACCENT_BENCH_FOLLOW").ok();
    let outline = std::env::var("ACCENT_BENCH_OUTLINE").ok();
    let close = std::env::var("ACCENT_BENCH_CLOSE").is_ok();
    let hidden = std::env::var("ACCENT_BENCH_HIDDEN").is_ok();
    let layout = std::env::var("ACCENT_BENCH_LAYOUT").ok();
    let collapse = std::env::var("ACCENT_BENCH_COLLAPSE").is_ok();
    let diagram = std::env::var("ACCENT_BENCH_DIAGRAM").ok();
    let clip = std::env::var("ACCENT_BENCH_CLIP").ok();
    let transfer = std::env::var("ACCENT_BENCH_TRANSFER").ok();
    let menu = std::env::var("ACCENT_BENCH_MENU").ok();
    let drop = std::env::var("ACCENT_BENCH_DROP").ok();
    let watch = std::env::var("ACCENT_BENCH_WATCH").ok();
    let tags = std::env::var("ACCENT_BENCH_TAGS").ok();
    let replace = std::env::var("ACCENT_BENCH_REPLACE").is_ok();
    let search = std::env::var("ACCENT_BENCH_SEARCH").ok();
    let find = std::env::var("ACCENT_BENCH_FIND").ok();
    let diag = std::env::var("ACCENT_BENCH_DIAG").ok();
    let save_as = std::env::var("ACCENT_BENCH_SAVE_AS").ok();
    let attach = std::env::var("ACCENT_BENCH_ATTACH").ok();
    if expand.is_none()
        && attach.is_none()
        && save_as.is_none()
        && find.is_none()
        && diag.is_none()
        && tags.is_none()
        && !replace
        && search.is_none()
        && clip.is_none()
        && transfer.is_none()
        && menu.is_none()
        && drop.is_none()
        && watch.is_none()
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
        && numbers.is_none()
        && reveal.is_none()
        && follow.is_none()
        && outline.is_none()
        && layout.is_none()
        && !collapse
        && git.is_none()
        && keys.is_none()
        && chrome.is_none()
        && !templates
        && !paths
        && !shell_keys
        && !term
        && hold.is_none()
        && picture.is_none()
        && image_look.is_none()
        && preview_look.is_none()
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
        if collapse {
            return bench_collapse(&app);
        }
        if let Some(rel) = compare {
            if let Some(rel) = rel.strip_prefix("lines:") {
                return bench_compare_lines(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("row:") {
                return bench_compare_row(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("diag:") {
                return bench_compare_diag(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("gutter:") {
                return bench_compare_gutter(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("conflict:") {
                return bench_compare_conflict(&app, rel);
            }
            return match rel.strip_prefix("pads:") {
                Some(rel) => bench_compare_pads(&app, rel),
                None => bench_compare(&app, &rel),
            };
        }
        if let Some(rel) = pdf {
            if let Some(rel) = rel.strip_prefix("pages:") {
                return bench_pdf_pages(&app, rel);
            }
            if let Some(arg) = rel.strip_prefix("bookmarks:") {
                return bench_pdf_bookmarks(&app, arg);
            }
            if let Some(rel) = rel.strip_prefix("strip:") {
                return bench_pdf_strip(&app, rel);
            }
            return match rel.strip_prefix("stale:") {
                Some(rel) => bench_pdf_stale(&app, rel),
                None => bench_pdf(&app, &rel),
            };
        }
        if drawing {
            return bench_drawing(&app);
        }
        if let Some(rel) = transfer {
            return bench_transfer(&app, &rel);
        }
        if let Some(rel) = clip {
            if let Some(dir) = rel.strip_prefix("outside:") {
                return bench_clip_outside(&app, dir);
            }
            return bench_clip(&app, &rel);
        }
        if let Some(rel) = save_as {
            return bench_save_as(&app, &rel);
        }
        if let Some(arg) = attach {
            return bench_attach(&app, &arg);
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
        if let Some(rel) = find {
            return bench_find(&app, &rel);
        }
        if let Some(rel) = diag {
            return bench_diagnostics(&app, &rel);
        }
        if let Some(rel) = menu {
            if let Some(rel) = rel.strip_prefix("press:") {
                return bench_menu_press(&app, rel);
            }
            return bench_menu(&app, &rel);
        }
        if let Some(arg) = drop {
            return bench_drop(&app, &arg);
        }
        if let Some(arg) = watch {
            return bench_watch(&app, &arg);
        }
        if let Some(arg) = diagram {
            return bench_diagram(&app, &arg);
        }
        if let Some(rels) = tabs {
            if let Some(rels) = rels.strip_prefix("pin:") {
                return bench_pin(&app, rels);
            }
            if rels == "pins" {
                return bench_pins_restored(&app);
            }
            if let Some(rels) = rels.strip_prefix("pinwin:") {
                return bench_pin_window(&app, rels);
            }
            return bench_tabs(&app, &rels);
        }
        if let Some(rel) = occur {
            return bench_occurrences(&app, &rel);
        }
        if let Some(rel) = theme {
            return bench_theme(&app, &rel);
        }
        if let Some(rels) = numbers {
            return bench_numbers(&app, &rels);
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
        if let Some(step) = hold {
            return bench_hold(&app, &step);
        }
        if let Some(arg) = picture {
            return bench_image(&app, &arg);
        }
        if let Some(rels) = image_look {
            return bench_image_look(&app, &rels);
        }
        if let Some(rel) = preview_look {
            return bench_preview_look(&app, &rel);
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
            if let Some(rels) = notes.strip_prefix("keys:") {
                return bench_chrome_keys(&app, rels);
            }
            return bench_chrome(&app, &notes);
        }
        if let Some(arg) = keys {
            if let Some(rel) = arg.strip_prefix("occur:") {
                return bench_occurrence_keys(&app, rel);
            }
            if let Some(rel) = arg.strip_prefix("box:") {
                return bench_box_drag(&app, rel);
            }
            return match arg.as_str() {
                "1" => bench_keys(&app),
                rel => bench_list(&app, rel),
            };
        }
        if let Some(arg) = git {
            if let Some(phase) = arg.strip_prefix("close:") {
                return bench_git_close(&app, phase);
            }
            if let Some(rel) = arg.strip_prefix("markers:") {
                return bench_git_markers(&app, rel);
            }
            return match (arg.strip_prefix("press:"), arg.as_str()) {
                (Some(path), _) => bench_git_press(&app, path),
                (None, "init") => bench_git_init(&app),
                (None, "sync") => bench_git_sync_over_fetch(&app),
                (None, "rebase") => bench_git_rebase(&app),
                (None, "focus") => bench_git_focus(&app),
                (None, "switch") => bench_git_switch(&app),
                _ => bench_git(&app),
            };
        }
        if let Some(rel) = style {
            if let Some(rel) = rel.strip_prefix("dragfold:") {
                return bench_drag_fold(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("seam:") {
                return bench_seam(&app, rel);
            }
            return match rel.strip_prefix("wrap:") {
                Some(rels) => bench_wrap(&app, rels),
                None => bench_style(&app, &rel),
            };
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
        glib::timeout_add_local_once(Duration::from_millis(1500), move || {
            bench_switcher_rows(&app);
            bench_quit(&app)
        });
    });
}

/// The first rows the switcher shows for the query, and whether each is a file, a note a link
/// names that is not written yet, or a note found by an alias.
fn bench_switcher_rows(app: &Rc<App>) {
    let list = app.window.visible_dialog().and_then(|d| {
        find_widget(d.upcast_ref(), &|w| w.is::<gtk::ListView>()).and_downcast::<gtk::ListView>()
    });
    let Some(model) = list.and_then(|l| l.model()) else {
        return;
    };
    for i in 0..model.n_items().min(5) {
        let Some(boxed) = model.item(i).and_downcast::<glib::BoxedAnyObject>() else {
            continue;
        };
        match &**boxed.borrow::<Rc<crate::palette::Item>>() {
            crate::palette::Item::File(rel) => println!("bench switcher_row file {rel}"),
            crate::palette::Item::Missing(rel) => println!("bench switcher_row missing {rel}"),
            crate::palette::Item::Alias { name, rel } => {
                println!("bench switcher_row alias {name} {rel}")
            }
            _ => {}
        }
    }
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
