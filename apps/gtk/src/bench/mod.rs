//! The `ACCENT_BENCH_*` drills: headless runs under Xvfb that time or probe one interaction,
//! print what they saw to stdout and quit. `install_bench_hooks` says which variable starts which.
//! The drills live in a module per area and share the helpers at the end of this one.

use super::*;

mod answer;
mod attach;
mod chrome;
mod compare;
mod complete;
mod corpus;
mod diagnostics;
mod diagram;
mod dismiss;
mod export;
mod files;
mod find;
mod folders;
mod git;
mod ignored;
mod image;
mod info;
mod keys;
mod loose;
mod memory;
mod minimap;
mod outline;
mod panes;
mod pdf;
mod present;
mod replace;
mod scroll;
mod search;
mod start;
mod style;
mod suggest;
mod synctex;
mod tags;
mod toasts;

use answer::{bench_answer, bench_answer_same};
use attach::bench_attach;
use chrome::{bench_chrome, bench_chrome_find, bench_chrome_keys};
use compare::{
    bench_compare, bench_compare_clicks, bench_compare_conflict, bench_compare_diag,
    bench_compare_folds, bench_compare_gap, bench_compare_gutter, bench_compare_large,
    bench_compare_left, bench_compare_lines, bench_compare_merge, bench_compare_pads,
    bench_compare_page, bench_compare_pick, bench_compare_press, bench_compare_reader,
    bench_compare_row, bench_compare_runaway, bench_compare_session, bench_compare_stale,
    bench_compare_typing, bench_compare_unfold,
};
use diagnostics::bench_diagnostics;
use diagram::bench_diagram;
use export::bench_export;
use files::{
    bench_clip, bench_clip_outside, bench_close, bench_hidden, bench_paths, bench_save_as,
    bench_templates, bench_transfer,
};
use find::bench_find;
use folders::{
    bench_drop, bench_expand, bench_menu, bench_menu_press, bench_move, bench_unfold, bench_watch,
};
use git::{
    bench_git, bench_git_branch, bench_git_close, bench_git_commit_focus, bench_git_focus,
    bench_git_init, bench_git_markers, bench_git_press, bench_git_rebase, bench_git_scroll,
    bench_git_switch, bench_git_sync_all, bench_git_sync_over_fetch,
};
use image::{bench_image, bench_image_look, bench_preview_look};
use info::bench_info;
use keys::{
    bench_box_drag, bench_hold, bench_keys, bench_list, bench_menu_caret, bench_occurrence_keys,
    bench_shell_keys, bench_term,
};
use minimap::bench_minimap;
use outline::bench_outline;
use panes::{
    bench_apart, bench_back, bench_collapse, bench_cycle, bench_layout, bench_layout_pick,
    bench_panes, bench_pin, bench_pin_window, bench_pins_restored, bench_reload, bench_tabs,
    bench_tree,
};
use pdf::{
    bench_drawing, bench_pdf, bench_pdf_bookmarks, bench_pdf_broken, bench_pdf_closed,
    bench_pdf_comments, bench_pdf_deep, bench_pdf_dropped, bench_pdf_failed, bench_pdf_insert,
    bench_pdf_pages, bench_pdf_renaming, bench_pdf_render, bench_pdf_stale, bench_pdf_strip,
    bench_sketch,
};
use replace::bench_replace;
use scroll::bench_scroll;
use search::bench_search;
pub(crate) use start::bench_start;
use style::{
    bench_drag_fold, bench_follow, bench_listing, bench_numbers, bench_occurrences, bench_reveal,
    bench_seam, bench_style, bench_theme, bench_typing, bench_wrap,
};
use tags::bench_tags;

/// `ACCENT_BENCH_EXPAND=<rel_path>` and `ACCENT_BENCH_SWITCHER=<query>` time the two interactions
/// that used to stall the main loop, print the numbers to stdout and quit, the switcher with the
/// first rows its query shows. Both run headless under
/// Xvfb, so "expanding a big directory is still fast" stays a command anyone can re-run rather
/// than a claim in a commit message. `RUST_LOG=accent=debug` adds the per-query breakdown.
/// `ACCENT_BENCH_SWITCHER=dismiss:<relA>,<relB>` clicks outside each dialog through XTEST
/// instead (`dismiss::bench_dismiss`), `=prefs` times Preferences presenting (`bench_prefs`), and
/// `=ignored:<rel>` follows a note in a gitignored folder into the window (`ignored::bench_ignored`),
/// and `=early:<query>` is the list landing in a dialog already open (`corpus::bench_early`).
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
/// row's buttons are out. `=scroll` clicks Stage half way down the scrolled changes list, then
/// commits with the history scrolled, and prints whether the rows on screen stayed put (see
/// `git::bench_git_scroll`). `=commit` commits the one change left through XTEST and prints
/// where the keyboard went (see `git::bench_git_commit_focus`). `ACCENT_BENCH_START=1`, on a
/// `--new-window` launch, is the start screen's, which has no `App` (`start::bench_start`).
/// `=switch` picks the second repository and clicks the history's first row
/// at once, and prints what that asked for, then what each pick draws at once, every repository
/// twice over (see `git::bench_git_switch`). `=markers:<rel>` resolves the conflict blocks a merge
/// left in a note through their buttons and the palette (see `git::bench_git_markers`). `=branch`
/// types names git would refuse into Create Branch… and prints the name the dialog says it will
/// create, then creates one and prints the branch HEAD is on. `=syncall` presses Sync All and
/// prints its spinner, its toast and each repository's state after it (see
/// `git::bench_git_sync_all`).
/// `ACCENT_BENCH_KEYS=1` likewise for the editor's key semantics, and prints text and caret
/// positions; `=<rel_note>` instead presses Return and Tab at the end of every list line of that
/// note and prints the ones whose marker or indent did not come out as `typing` says it should,
/// plus the width one indent is worth there, then Tab on lines that already have text on them.
/// `=occur:<rel>` asks for XTEST presses of `Alt+J`, `Ctrl+Shift+L` and `Shift+Alt+Up` / `Down`
/// with typing after them, and prints every selection and the buffer after each (see
/// `keys::bench_occurrence_keys`). `=box:<rel>` asks for XTEST drags and a press with `Shift+Alt`
/// and without, and prints the carets and the selection after each (see `keys::bench_box_drag`).
/// It opens with the completion popup: that Return at the end of a list item under it continues
/// the list while no row is selected and accepts the row once one is, and that a view taken off
/// screen takes the popup with it.
/// `ACCENT_BENCH_SUGGEST=escape:<rel>` types a word for the popup and paints ghost text, with the
/// find bar open and with the note compared with its disk copy, and asks for an XTEST Escape over
/// each, printing what it put away (see `suggest::bench_suggest_escape`). Only on a scratch vault
/// under `/tmp`. `=ghost:<rel>` prints the status bar's busy line from a cold start, then switches
/// Ghost Text off and on under the open note and prints the `merl-rt` processes, how long one
/// takes to end and what the line says (see `suggest::bench_suggest_ghost`). `=words:<rel>`
/// switches Word Suggestions on, off and on and prints the words offered and whether a word typed
/// by XTEST brings the popup up (see `suggest::bench_suggest_words`), on a scratch vault under
/// `/tmp`. `ACCENT_BENCH_COMPLETE=note:<rel>` types `[[` and `#` completions into a note and
/// `=code:<rel>` member completion into a file a language server answers for, and prints the
/// rows, the documentation and the text each key leaves (see `complete::bench_complete`).
/// `ACCENT_BENCH_CHROME=1` fires actions at a faded window and prints whether the
/// chrome stayed away; `=<relA>,<relB>` then opens the two notes side by side, prints what each
/// focus level fades, and holds the line fade on screen and times it. `=keys:<note>,<pdf>` asks
/// for XTEST presses of the keys that step through a note, the preview and a PDF, and prints
/// whether each one faded the chrome (see `chrome::bench_chrome_keys`). `=find:<a>,<b>,<c>`
/// presses a tab in the other of two panes' bar, opens a note open there again and deletes the one
/// in front of it, through XTEST, and prints the active pane beside the one with the keyboard,
/// which pane recedes at High and whose find bar `Ctrl+F` opens (see `chrome::bench_chrome_find`).
/// `=present:<note>,<pdf>,<image>,<side>` presents a note, a PDF, an image and a shell from one of
/// two panes and prints what F5 shows, the find bar, a held `Ctrl+Tab`'s card, Escape over it, a
/// toast against the status bar, and the layout leaving F5 puts back (see
/// `present::bench_present`). `=toasts` raises four toasts and two under one key and prints the
/// ones standing (see `toasts::bench_toasts`). `ACCENT_BENCH_PATHS=1`
/// drives a path entry's completion, and prints widths and the text its keys apply.
/// `ACCENT_BENCH_STYLE=<rel_path>` types a heading into a note at two sizes and prints whether it
/// was styled on the keystroke or on the debounce, then whether a copy and paste, a middle click
/// or a drop out of a styled or folded line brings its tags along, and last whether a run of edits
/// leaves every tag where a fresh pass puts it (`mismatch=0`). `=typing:<rel>` types into 4 to
/// 256 KB of note and prints the main thread's busy share (see `style::bench_typing`).
/// `=wrap:<rel>,<rel>…` opens each file in a narrow window and prints where every line's wrapped
/// rows hang, then times the wrap indent on 10k lines of code in the last one. `=dragfold:<rel>`
/// selects a folded section and prints where to press and let go for XTEST, then what a real drag
/// of it left in the note.
/// `=seam:<rel>` joins a line to a fold with Delete and with Backspace, asks for the iter at every
/// pixel row, and prints what stays hidden: a line left partly hidden aborts it inside GTK; then
/// runs a Ctrl-held pointer over a fold shut in the same frame (`case=stale`), draws the view
/// with its top row on a partly hidden line (`case=screen`), and draws it at the top over hidden
/// first lines with the layout's height overflowed below zero (`case=overflow`).
/// `=listing:<rel>` prints how a LaTeX file's listings are coloured (`style::bench_listing`).
/// Every form of it runs only on a scratch vault under `/tmp` (`scratch_only`).
/// `ACCENT_BENCH_PANES=<relA>,<relB>` moves a tab between panes and prints where it landed, then
/// steps the split it leaves with Move Divider from a dragged 47 % and prints the share each time.
/// `ACCENT_BENCH_COMPARE=<rel_path>` compares a note with its disk copy inside its tab and prints
/// what the panes hold and whether their rows line up, and what focus mode's line fade follows in
/// the other column. `=pads:<rel_path>` instead stages a note of long paragraphs in a repository
/// it makes itself and types at the start of the two lines whose padding tag does not begin at
/// the newline before them, and `=lines:<rel_path>` stages and
/// unstages one line of a note it commits in a repository of its own. `=row:<repo_rel>` activates
/// that file's Changes row, as a click on it does, and prints what the comparison it opened holds;
/// `=row:stale:<repo_rel>` stages the file behind the pane's back first, so the row it activates
/// is one git has outgrown and the comparison would have nothing to show. `=row:staged:<repo_rel>`
/// and `=row:commit:<repo_rel>` are the same shape for the two that open a tab of their own: a
/// Staged row unstaged behind the pane's back, and the file at HEAD against HEAD~1 where HEAD did
/// not touch it; where HEAD did, the tab it opens says whether its first change is on screen and
/// where the shared scrollbar is. `=pick:<repo_rel>` picks a second repository in the chooser and
/// clicks the root's row at once, printing both sides' line counts of what opened (see
/// `compare::bench_compare_pick`). `=clicks` clicks Changes, Staged, Deleted and history rows in
/// a repository it makes, in the orders and at the moments a reader does, and prints what both
/// columns of the comparison in front hold after each (`compare::bench_compare_clicks`);
/// `=stale:<rel>` clicks a row whose file's tab is behind the disk (`compare::bench_compare_stale`).
/// `=diag:<rel_text_file>`
/// collapses a run with warnings in it and prints how many end-of-line messages and gutter marks
/// each state drew: the messages of a hidden run go, the icons stay. It then folds a block over
/// the same file, which hides lines the same way, and reads the two numbers again without
/// publishing anything: a fold's header keeps its own message, the lines under it do not.
/// `=gutter:<rel_text_file>` puts a warning and a fold chevron on a line padded below and on one
/// padded above, prints each one's cell and first row, and holds the window up for a screenshot.
/// `=conflict:<rel_text_file>` writes a sync conflict copy beside that file while its tab is open
/// and prints what the banner stands for: live, after the tab is opened again, and once the copy
/// is gone. `=left:<rel>` leaves a comparison with the disk copy and scrolls the editor once the
/// copy's column is freed (see `compare::bench_compare_left`). `=folds:<rel>` compares a note
/// whose folded section ends inside a collapsed run and asks for the iter at every pixel row
/// (`compare::bench_compare_folds`). `=gap:<rel>` opens hidden runs from their buttons in a long
/// note, and then in a tab of two blobs, and prints where the rows around each button went
/// (`compare::bench_compare_gap`). `=page:<rel>` prints the sticky title and the page the
/// companion shares with the editor, across a zoom and a new Indent Width
/// (`compare::bench_compare_page`). `=press:<rel>` prints where to press a "⋯" button and a hunk's
/// Take and Keep Both in the strip with XTEST, and where the carets are after each press
/// (`compare::bench_compare_press`).
/// `=reader:<rel>` prints where to turn the wheel, press Page Down or Ctrl+End and drag a scrollbar
/// or the minimap with XTEST while the comparison is still on its way to its first hunk, and
/// whether it took the scroll back or held the line it left at the top
/// (`compare::bench_compare_reader`).
/// `=merge:<rel>` merges two branches that conflict over `<rel>` in a repository it makes, opens
/// the merge view from the Merge Conflicts row and takes each block's sides from the buttons on
/// its band, printing where to press them with XTEST (`compare::bench_compare_merge`).
/// `=unfold:<rel>` presses Show All Unchanged Lines and lets it go, in the note's tab and in a tab
/// of two blobs, printing the hidden runs and the line at the top of the view each time, or the
/// caret's line where it is on screen, then drags the divider between the columns
/// (`compare::bench_compare_unfold`).
/// `=runaway:<rel>` lays a comparison again eight times before GTK lays out what it re-padded,
/// printing the padding of a paragraph under a blank line after each
/// (`compare::bench_compare_runaway`). `=typing:<rel>` types by XTEST into and around a run opened
/// in a long working-tree comparison, `=typing:short:<rel>` in one under 16 KB, printing what the
/// frames meanwhile showed of the line typed into and its partner
/// (`compare::bench_compare_typing`). `=session:save:<rel>,<other>` then `=session:back:…` on
/// one scratch home leave comparisons with git open across a restart and print what came back
/// (`compare::bench_compare_session`). `=large:<rel>` compares a long file with a copy changed
/// throughout and prints how long each step held the main loop (`compare::bench_compare_large`).
/// `ACCENT_BENCH_MEMORY=<note>,<code>,<pdf>[,<rounds>]` reads the PDF through, hides and closes it
/// and prints what the process holds after each, then opens and closes every kind of tab, a
/// comparison, the preview, a shell and a window, and prints what outlived its close and how the
/// resident size moved (`memory::bench_memory`). Only on a scratch vault under `/tmp`.
/// `ACCENT_BENCH_WEBIDLE=<note>` shortens how long an unused WebKit view keeps its process to two
/// seconds, and prints the WebKit processes as the preview and the diagram formulas' typesetter
/// are used, left and used again (`memory::bench_webidle`). It writes two diagrams, so point it at
/// a scratch vault.
/// `ACCENT_BENCH_IMAGE=<rel_png>,<rel_other_png>` zooms an image and replaces its file with one of
/// another size, printing what the picture asks for and says either side of the reload;
/// `=zoom:<rel_svg>,…` steps an SVG in and back, printing what it is drawn from as the zoom settles.
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
/// name, and writes a one-page document over the new name from outside, which the reader must
/// follow. It writes to the document and moves it, so point it at a scratch copy; and point it at a
/// document of several pages, since a one-page PDF is wholly on screen whatever the scroll offset
/// was. `=stale:<rel_path>` is the remote vault's etag gate: it stamps the cached copy with an
/// etag the host never had, adds a page and prints whether the host's copy is untouched, what
/// `<name> (edited).pdf` beside it holds and which of the two the tab is on, then adds another
/// and prints the same again — the page must go into that copy rather than a numbered one, and
/// leave the toast count where the refusal put it. `=failed:<rel_path>` makes the document and its folder
/// read-only on the host, adds two pages and prints the toast count after each (one failure said,
/// the second quiet), then makes them writable and presses the toast's Retry, which sends both,
/// and fails once more, which is said again; then writes a one-page document over it on the host
/// while that page has not gone up, and prints where the page went and which file the tab is on;
/// each `chmod` adds the host's "Indexed …" toast to the count.
/// `=dropped:<rel_path>` writes a note linking into pages 1 to 3, adds a page and ends the vault's
/// ssh master at once, holds the link down while the save and the links' rewrite land, and prints
/// what was said, then reconnects and prints whether the page reached the host and the links
/// followed it. `=renaming:<rel_path>` adds a page and renames the document while its
/// upload is still out (point it at a PDF of a few megabytes on a remote vault), then prints
/// whether the old name came back on the host and where the page landed. `=closed:<rel_path>`
/// adds a page and closes the tab in the same turn, then prints what the vault holds; on a remote
/// vault it does the same with the host's folder read-only, then opens the document again with
/// the folder writable, which must send that page.
/// `=pages:<rel_path>` moves the first page below the third as
/// a drop in the thumbnail strip does, adds a page before the one being read and one after the last
/// page, and deletes the first through the window actions, which asks nothing, then walks
/// all four back with Undo and forward again with Redo, and prints the page being read and each
/// page's text on disk after every step, with the pages a note written first links to (a
/// highlight, a jump and a markdown link into pages 1 to 3) and what the toast said; then the
/// items of the page's own menu without and with a selection, and of the status bar's page
/// count's menu.
/// Point it at a scratch copy of the generated vault's `Attachments/pages.pdf`.
/// `=insert:<rel_path>,<source>` drops the PDF `source` onto `rel_path` every way there is: its
/// row from the Files tree onto the pages through XTEST, the line watched, as a file from another
/// application onto the pages, and both onto `rel_path`'s row, each undone, printing the pages on
/// disk, the links of a note into it and the toasts after each (`pdf::bench_pdf_insert`). Both at
/// the root of a scratch vault, copies of the generated vault's `Attachments/pages.pdf` and
/// `Attachments/comments.pdf`, whose pop-up pdfium's copy left pointing into it.
/// `=strip:<rel_path>` is the pointer's half, held for XTEST: it opens the document with the
/// Outline pane up and prints the same every two seconds for 40 s, so a hover and a drag along the
/// thumbnail strip (`build-aux/xtest.py :N "drag X0 Y0 X1 Y1"`) can be watched landing in the
/// file. `=bookmarks:<rel_path>[,<rel_diagram>]` scrolls through the document with the Outline pane
/// up and prints the bookmark each page is under, whether it is in view and who has the keyboard,
/// then the same after a page edit and whether the list is the one it was; with a diagram, the
/// same for each of its pages (`pdf::bench_pdf_bookmarks`). `=render:<rel_path>` scrolls the
/// reading view and the thumbnail strip together in bursts and zooms, and after each prints how
/// long both took to paint everything they want, or what is still missing five seconds after the
/// last tile landed, then `stuck=<bursts that never finished>` (`pdf::bench_pdf_render`); point
/// it at a few hundred heavy pages, which is where one view's batch used to drop the other's.
/// `=broken:<rel_path or absolute path>` cuts the PDF in half in place, opens it and writes it
/// back slowly, then breaks and mends it again under the open tab, printing the waiting page or
/// the pages after each step and how many opens that cost (`pdf::bench_pdf_broken`); it writes
/// into the file, so point it at a scratch copy. `=deep:<rel_path>[,<percent>]` zooms to 800 % or
/// `percent` across the break between the first two pages and prints, each second for 20 s, the
/// tiles wanted, those on screen not yet sharp, and those that landed and landed again; then how
/// many wheel notches and Page Downs at that zoom arrived on a tile not yet sharp
/// (`pdf::bench_pdf_deep`); point it at a text document of a few pages. `=synctex:<rel_path>`
/// goes to the source from points of a LaTeX build's first two pages through Go to Source, as
/// the page's menu does, and back with Show in PDF from the line it landed on, printing each
/// line and line of text, or the toast (`synctex::bench_synctex`); point it at a `-synctex=1`
/// build of an article in a scratch vault. A build without a SyncTeX file prints the palette's
/// rows for both commands and the `.tex` tab's menu each second for eight instead, for a
/// secondary press through XTEST to turn its Show in PDF into "(no SyncTeX data)".
/// `=comments:<rel_path>` writes a note linking to the first word of the generated vault's
/// `Attachments/comments.pdf` (`rel_path`), then prints, over each comment and each mark without
/// one, what the tooltip shows, what a click pins and whether a tooltip shows after it; the same
/// with the pen in hand, and over the note's highlight with the Eraser in hand, which opens
/// nothing, and with none, which opens the note; then the comments
/// once Export Highlights has written the note's quote in, the Outline pane's list of them, and
/// where its row of the last page's comment goes and what it pins (`pdf::bench_pdf_comments`). It
/// writes a note and the PDF, so point it at a scratch vault. `=walk:<rel_path>` times reading
/// every page's comments for that list against the first screen painting, and how soon closing
/// the tab ends it (`pdf::bench_pdf_walk`); point it at a few hundred pages.
/// `ACCENT_BENCH_DRAWING=1` fires New Drawing at the vault root, prints what the dialog came up
/// with, answers it with the window-shaped size and prints the file that landed and the tool the
/// tab it opened has in hand, and what the vault holds under its name. `=sketch:<rel_note>` fires
/// Insert Sketch in that note and prints how long the main loop was held, whether the note embeds
/// what it made, the tab it opened and what the vault holds under that name.
/// `ACCENT_BENCH_TABS=<rel_note>,<rel_pdf>` walks a note, a shell and a PDF through one pane and
/// closes the lot, printing what the find bar and the Outline pane say at each step: what a tab
/// switch and the last tab's close leave behind. The note opens as a preview and is kept by its
/// eye first, and its title and indicator are printed either side of that.
/// `=pin:<a>,<b>,<c>,<d>` opens four notes in one pane and prints each pane's tabs, pinned ones
/// marked `^`, after every step: `c` pinned from its tab menu and `b` from the palette, `c`
/// unpinned and pinned again, `a` and then `b` moved across the pinned ones as a drag along the
/// bar ends, and `d` and then `c` moved right with Move Tab. It quits the way Ctrl+Q does, which
/// writes the session; `=pins` on the same scratch state prints what the restore brought back.
/// `=reload:<key>,…` opens those files, types into the one in front, resizes the window and fires
/// Reload Window, then prints the panes, the size and whether the typing was written, before and
/// in the window that comes back (see `panes::bench_reload`); any window will do: a vault's, one
/// opened on a file, `--terminal` or `terminal://<name>`.
/// `=tree:<a>,<b>,<c>` clicks `a` and then `b` in the Files tree, double-clicks `c` and then
/// `a`, drags `b` onto the right edge of the pane, all through XTEST, then pins `c` and fires
/// Close Tabs in Pane over the left pane, and prints each pane's tabs after every step: the vault
/// root has to list the three, as a scratch vault of three notes does. Under
/// `ACCENT_BENCH_SLOW_READ=<ms>`, which holds every text read that long on its worker as a slow
/// host would, each double click's second press comes before its tab, which must land kept.
/// `=edited:<pdf>,<diagram>,<note>` opens the PDF as a click on its tree row does, draws a stroke
/// across it with the pen through XTEST, opens the diagram the same way and adds a page to it,
/// then opens the note, and prints the pane's tabs after each: an edit keeps a preview, so neither
/// the PDF nor the diagram may be replaced, which would take its Undo with it.
/// `=pinwin:<a>,<b>,<c>` pins `a` among three notes, then hands `b` and then `a` to the window
/// kept for loose files the way a drop there does, and prints both windows' tabs after each. On a
/// remote vault it waits for the host, and neither tab may leave: the file is on the host.
/// `=apart:<rel>,…` opens those files, a note it makes (`apart-drill.md`) and a shell; fires Open
/// in New Window from the first one's tree menu, then Move to New Window from every tab's menu,
/// the made note with an edit still in its buffer, printing whether each menu offered its item,
/// whether the tab stayed and what was said; then every other window and what it holds, and what
/// the made note says on disk, before removing it. On a remote vault neither item is offered.
/// `=cycle:<a>,<b>,…` opens those files and a shell in one pane, prints `cycle ready` once the
/// window has the keyboard (`xtest.py :N "move 700 400; focus"`), then for 25 s prints the held
/// `Ctrl+Tab` chord's state whenever it changes, with the ms since the chord began: whether the
/// card is up, its rows with the lit one, the tab in front and the pane's order; and once, where
/// the card's third row is (`cycle aim row2=x,y`), for a click on it with Ctrl held.
/// `=back:<a>,<b>` puts the caret on line 10 of `a`, goes to `b`, writes three lines above that
/// place in `a` and goes Back, printing where the caret landed against where the text it left
/// went; then Forward, `a`'s whole text replaced as a reload replaces it, and Back again, which
/// lands on the line and column the place was taken at.
/// `=loose:<rel_pdf>` writes a note and its images into `loose-drill/` and opens the note in a
/// window of its own from its tree row, where no vault is behind it, and prints what the Outline
/// pane there holds and the headings it lists, then whether each image in its preview loaded:
/// those beside it and under it, and neither one above its folder nor one through a symlink out
/// of it. Then it opens an image and a copy of the PDF in windows of their own, the PDF here too,
/// and prints the image's size there before and after the file is written over with a larger one,
/// and the pages of the PDF there before and after a page is added here, then here after one is
/// added there (`loose::bench_loose`), before removing the folder.
/// `ACCENT_BENCH_FOLLOW=<rel_note>` puts the pointer on a wikilink, on a plain word and on a bare
/// URL with Ctrl held, and prints what the Ctrl+hover underline covers and the URL under the caret;
/// then it follows a link nothing answers to from the caret, as F12 does, and prints the dialog
/// that offers to create it, with the name it arrives prefilled with. Then `[[#Nowhere]]`, which
/// must put the caret on the note's first line and toast, as must a preview click on
/// `[[#Elsewhere]]`; `[[#^blk]]` followed both ways, which must land on the block it marks; and a
/// link typed at the end of a note past 16 K characters and followed at once, which must offer New
/// File as well. Any text file will do for the underline: a `.txt` has no wikilinks, and its URL
/// underlines all the same. `=hover:<rel_note>` instead aims the real pointer at the note's first
/// wikilink and prints the hover's font and size beside the note's and the window's, whether it
/// survives being emptied while up, and whether a wheel scrolls it (see `style::bench_hover`).
///
/// `ACCENT_BENCH_OUTLINE=<rel_note>,<rel_other>` walks the caret down a note and prints which
/// Outline row is selected, whether it is in view and who has the keyboard; then again after a
/// switch to `<rel_other>` and back, and after the caret moved while the pane was hidden; last
/// the list's scroll and row through a few edits, which leave them alone, and one caret move.
/// `=hold:<rel_note>` prints where to aim and then the pane's state as XTEST drives it, for 20 s.
///
/// `ACCENT_BENCH_OCCUR=<rel_note>` selects things in a note and prints what the muted occurrence
/// highlight made of each selection, plus the two match colours and the priorities of the tags
/// they are painted with. Only on a scratch vault under `/tmp`, as `ACCENT_BENCH_STYLE`.
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
/// hues a CSV's columns and the git lanes share hold after every switch: as its first frame paints
/// them, and again 300 ms later, the two agreeing. A second
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
/// `ACCENT_BENCH_ANSWER=<rel_note>` answers Overwrite, Keep Mine and a deleted file's Save over a
/// note it changes behind the tab's back, and prints how long each held the main loop
/// (`answer::bench_answer`). `=same:<rel_note>` moves the file's etag alone and types at once,
/// and prints whether the banner went up (`answer::bench_answer_same`).
/// `ACCENT_BENCH_EXPORT=pdf:<rel_pdf>` exports a PDF that a note highlights into `$TMPDIR` and
/// copies it for printing, and prints what each copy holds against the source
/// (`export::bench_export_pdf`); `=note:<rel_note>` exports a note as PDF and as HTML into
/// `$TMPDIR` and reads both back, then opens its print dialog (`export::bench_export_note`).
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
/// down; up; keyup ctrl"` — printing the rows drawn marked, how many documents are open and the
/// colour the row above it, it and the two below are painted in every five seconds, which is how
/// the modifier half is driven at all.
///
/// `ACCENT_BENCH_TAGS=<rel_note>` writes a marker tag into a note and takes it away again with
/// the Tags section on screen, printing whether its list holds the marker at each step.
///
/// `ACCENT_BENCH_INFO=<rel>[,<rel>…]` opens each file with the Info pane on screen and prints
/// which sections it shows, open or shut, and what they say (`info::bench_info`).
///
/// `ACCENT_BENCH_REPLACE=1` writes a note holding one unique word, presses the Search pane's
/// Replace All on it and prints what the pane lists before and after the rewrite.
///
/// `ACCENT_BENCH_FIND=<rel_note>` uses two queries in a note's find bar and then presses Ctrl+F
/// twice over the open bar — once on a typed query, once on one Up recalled — printing what is
/// selected in the box each time and again once its delayed search has run. `=options` writes a
/// note of its own and prints what the bar reads out under each of its toggles, an invalid
/// pattern and a Replace All with `$1`, then removes the note.
///
/// `ACCENT_BENCH_DIAG=<rel_code_file>` hands a code tab an error, a warning and a hint, then
/// presses the status bar's count twice, printing what the count says and how much of the answer
/// the text is carrying each time; then types after a long message and prints whether it was cut
/// again for the line's new end. It writes the file: point it at a scratch vault.
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
/// `=label:<query>` finds `<query>` in a diagram's label on its second page and opens the row,
/// printing the rows' dim lines, which name the page, and the page and cell the diagram then shows;
/// `=label:<query>:xml` does it with the diagram saved as a plain `.xml`.
/// `=seed:<rel_note>` fires Ctrl+Shift+F and Ctrl+Shift+H over the note and prints which box has
/// the keyboard and what it has selected, then types after the real chord through XTEST (see
/// `search::bench_seed`).
///
/// `ACCENT_BENCH_HIDDEN=1` prints the Files pane's rows and which of them are dimmed, then toggles
/// Show Hidden Files off and on again, printing them after each.
///
/// `ACCENT_BENCH_UNFOLD=<rel>,<rel>,…` opens those folders in the Files tree as clicks would and
/// prints what it lists under each; `=race:<dir>`, `=renew:<dir>`, `=again:<dir>` and
/// `=tab:<rel>` change a folder the index does not walk under the tree and a tab from outside
/// accent and print whether they followed (`folders::bench_unfold`). Those write, so only on a
/// scratch vault.
///
/// `ACCENT_BENCH_MOVE=<rel>,<rel>,…` moves those paths with Move to…, past its dialog, into a
/// folder that is not there yet, then tries the two moves it refuses, and prints the dialog, the
/// toasts and where the files are (`folders::bench_move`). Only on a scratch vault under `/tmp`.
///
/// `ACCENT_BENCH_SCROLL=<rel_dir>` scrolls the Files tree, the Git pane's two lists, the Search
/// results and the Tags list half way down with the keyboard on a row, changes the vault and its
/// repository under them, and prints where each list is after every change
/// (`scroll::bench_scroll`). It makes the vault a repository, so only on a scratch vault under
/// `/tmp`.
///
/// `ACCENT_BENCH_DIAGRAM=<rel>` edits a diagram (a sample is written there if there is none) and
/// prints each step through the save; `=shot:<rel>:<dir>` paints every page into `<dir>`;
/// `=export:<rel>:<dir>` exports it as PDF, PNG and SVG and prints it into `<dir>` under Dark.
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
/// which tab is in front of the active pane, which the session writes as the active one, and how
/// many places each pane's Back and Forward hold. `=1` prints the tree a restore built
/// once its tabs have landed, and quits without writing one. `=pick:<rel>` does the same, having
/// selected `<rel>` in its pane as a click on its tab would, between two tabs landing;
/// `=focus:<rel>` gives it the keyboard instead, and `=open:<rel>` opens a note the session does
/// not hold before any tab has landed, as a reader would into the still-empty active pane. On a
/// remote vault they wait for the host to
/// answer, `<a>,…` and `=1` printing what the window shows until then, and `=quit` quits there the
/// way Ctrl+Q does.
///
/// `ACCENT_BENCH_MINIMAP=<round>:<rel>` is the minimap's (`minimap::bench_minimap`).
pub fn install_bench_hooks(app: &Rc<App>) {
    let expand = std::env::var("ACCENT_BENCH_EXPAND").ok();
    let minimap = std::env::var("ACCENT_BENCH_MINIMAP").ok();
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
    let memory = std::env::var("ACCENT_BENCH_MEMORY").ok();
    let webidle = std::env::var("ACCENT_BENCH_WEBIDLE").ok();
    let pdf = std::env::var("ACCENT_BENCH_PDF").ok();
    let drawing = std::env::var("ACCENT_BENCH_DRAWING").ok();
    let tabs = std::env::var("ACCENT_BENCH_TABS").ok();
    let occur = std::env::var("ACCENT_BENCH_OCCUR").ok();
    let suggest = std::env::var("ACCENT_BENCH_SUGGEST").ok();
    let complete = std::env::var("ACCENT_BENCH_COMPLETE").ok();
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
    let info = std::env::var("ACCENT_BENCH_INFO").ok();
    let search = std::env::var("ACCENT_BENCH_SEARCH").ok();
    let find = std::env::var("ACCENT_BENCH_FIND").ok();
    let diag = std::env::var("ACCENT_BENCH_DIAG").ok();
    let save_as = std::env::var("ACCENT_BENCH_SAVE_AS").ok();
    let attach = std::env::var("ACCENT_BENCH_ATTACH").ok();
    let answer = std::env::var("ACCENT_BENCH_ANSWER").ok();
    let export = std::env::var("ACCENT_BENCH_EXPORT").ok();
    let scroll = std::env::var("ACCENT_BENCH_SCROLL").ok();
    let moving = std::env::var("ACCENT_BENCH_MOVE").ok();
    let unfold = std::env::var("ACCENT_BENCH_UNFOLD").ok();
    if expand.is_none()
        && unfold.is_none()
        && scroll.is_none()
        && moving.is_none()
        && attach.is_none()
        && answer.is_none()
        && export.is_none()
        && save_as.is_none()
        && find.is_none()
        && diag.is_none()
        && tags.is_none()
        && !replace
        && info.is_none()
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
        && memory.is_none()
        && webidle.is_none()
        && pdf.is_none()
        && drawing.is_none()
        && tabs.is_none()
        && occur.is_none()
        && suggest.is_none()
        && complete.is_none()
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
        && minimap.is_none()
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
        if let Some(arg) = minimap {
            return bench_minimap(&app, &arg);
        }
        if let Some(arg) = memory {
            return memory::bench_memory(&app, &arg);
        }
        if let Some(note) = webidle {
            return memory::bench_webidle(&app, &note);
        }
        if let Some(rel) = compare {
            // These make a repository in the vault root, or stage and commit in the one there.
            if [
                "lines:", "row:", "left:", "pads:", "clicks", "typing:", "session:", "merge:",
            ]
            .iter()
            .any(|mode| rel.starts_with(mode))
            {
                own_repository_only(&app, "ACCENT_BENCH_COMPARE");
            }
            if let Some(rel) = rel.strip_prefix("lines:") {
                return bench_compare_lines(&app, rel);
            }
            if let Some(arg) = rel.strip_prefix("session:") {
                return bench_compare_session(&app, arg);
            }
            if let Some(rel) = rel.strip_prefix("row:") {
                return bench_compare_row(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("pick:") {
                return bench_compare_pick(&app, rel);
            }
            if rel == "clicks" {
                return bench_compare_clicks(&app);
            }
            if let Some(rel) = rel.strip_prefix("stale:") {
                return bench_compare_stale(&app, rel);
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
            if let Some(rel) = rel.strip_prefix("left:") {
                return bench_compare_left(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("folds:") {
                return bench_compare_folds(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("gap:") {
                return bench_compare_gap(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("page:") {
                return bench_compare_page(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("press:") {
                return bench_compare_press(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("reader:") {
                return bench_compare_reader(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("merge:") {
                return bench_compare_merge(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("unfold:") {
                return bench_compare_unfold(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("runaway:") {
                return bench_compare_runaway(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("typing:") {
                return bench_compare_typing(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("large:") {
                return bench_compare_large(&app, rel);
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
            if let Some(arg) = rel.strip_prefix("insert:") {
                return bench_pdf_insert(&app, arg);
            }
            if let Some(arg) = rel.strip_prefix("bookmarks:") {
                return bench_pdf_bookmarks(&app, arg);
            }
            if let Some(rel) = rel.strip_prefix("strip:") {
                return bench_pdf_strip(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("render:") {
                return bench_pdf_render(&app, rel);
            }
            if let Some(arg) = rel.strip_prefix("broken:") {
                return bench_pdf_broken(&app, arg);
            }
            if let Some(arg) = rel.strip_prefix("deep:") {
                return bench_pdf_deep(&app, arg);
            }
            if let Some(rel) = rel.strip_prefix("failed:") {
                return bench_pdf_failed(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("dropped:") {
                return bench_pdf_dropped(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("renaming:") {
                return bench_pdf_renaming(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("closed:") {
                return bench_pdf_closed(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("comments:") {
                return bench_pdf_comments(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("walk:") {
                return pdf::bench_pdf_walk(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("synctex:") {
                return synctex::bench_synctex(&app, rel);
            }
            return match rel.strip_prefix("stale:") {
                Some(rel) => bench_pdf_stale(&app, rel),
                None => bench_pdf(&app, &rel),
            };
        }
        if let Some(arg) = drawing {
            return match arg.strip_prefix("sketch:") {
                Some(rel) => bench_sketch(&app, rel),
                None => bench_drawing(&app),
            };
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
        if let Some(arg) = export {
            return bench_export(&app, &arg);
        }
        if let Some(arg) = attach {
            return bench_attach(&app, &arg);
        }
        if let Some(rel) = answer.as_deref().and_then(|a| a.strip_prefix("same:")) {
            return bench_answer_same(&app, rel);
        }
        if let Some(rel) = answer {
            return bench_answer(&app, &rel);
        }
        if let Some(rels) = info {
            return bench_info(&app, &rels);
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
        if let Some(dir) = scroll {
            own_repository_only(&app, "ACCENT_BENCH_SCROLL");
            return bench_scroll(&app, &dir);
        }
        if let Some(rels) = moving {
            return bench_move(&app, &rels);
        }
        if let Some(dirs) = unfold {
            return bench_unfold(&app, &dirs);
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
            if let Some(keys) = rels.strip_prefix("reload:") {
                return bench_reload(&app, keys);
            }
            if let Some(rels) = rels.strip_prefix("apart:") {
                return bench_apart(&app, rels);
            }
            if let Some(rels) = rels.strip_prefix("cycle:") {
                return bench_cycle(&app, rels);
            }
            if let Some(rels) = rels.strip_prefix("back:") {
                return bench_back(&app, rels);
            }
            if let Some(rels) = rels.strip_prefix("tree:") {
                return bench_tree(&app, rels);
            }
            if let Some(rels) = rels.strip_prefix("edited:") {
                return panes::bench_edited(&app, rels);
            }
            if let Some(pdf) = rels.strip_prefix("loose:") {
                return loose::bench_loose(&app, pdf);
            }
            return bench_tabs(&app, &rels);
        }
        if let Some(rel) = occur {
            scratch_only(&app, "ACCENT_BENCH_OCCUR");
            return bench_occurrences(&app, &rel);
        }
        if let Some(arg) = suggest {
            if let Some(rel) = arg.strip_prefix("escape:") {
                scratch_only(&app, "ACCENT_BENCH_SUGGEST");
                return suggest::bench_suggest_escape(&app, rel);
            }
            if let Some(rel) = arg.strip_prefix("ghost:") {
                return suggest::bench_suggest_ghost(&app, rel);
            }
            if let Some(rel) = arg.strip_prefix("words:") {
                scratch_only(&app, "ACCENT_BENCH_SUGGEST");
                return suggest::bench_suggest_words(&app, rel);
            }
            return bench_quit(&app);
        }
        if let Some(arg) = complete {
            return complete::bench_complete(&app, &arg);
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
            if let Some(rels) = notes.strip_prefix("find:") {
                return bench_chrome_find(&app, rels);
            }
            if let Some(rels) = notes.strip_prefix("present:") {
                return present::bench_present(&app, rels);
            }
            if notes == "toasts" {
                return toasts::bench_toasts(&app);
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
            if let Some(rel) = arg.strip_prefix("menu:") {
                return bench_menu_caret(&app, rel);
            }
            return match arg.as_str() {
                "1" => bench_keys(&app),
                rel => bench_list(&app, rel),
            };
        }
        if let Some(arg) = git {
            own_repository_only(&app, "ACCENT_BENCH_GIT");
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
                (None, "scroll") => bench_git_scroll(&app),
                (None, "commit") => bench_git_commit_focus(&app),
                (None, "switch") => bench_git_switch(&app),
                (None, "branch") => bench_git_branch(&app),
                (None, "syncall") => bench_git_sync_all(&app),
                _ => bench_git(&app),
            };
        }
        if let Some(rel) = style {
            scratch_only(&app, "ACCENT_BENCH_STYLE");
            if let Some(rel) = rel.strip_prefix("dragfold:") {
                return bench_drag_fold(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("seam:") {
                return bench_seam(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("listing:") {
                return bench_listing(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("typing:") {
                return bench_typing(&app, rel);
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
        if let Some(rels) = query.strip_prefix("dismiss:") {
            return dismiss::bench_dismiss(&app, rels);
        }
        if let Some(rel) = query.strip_prefix("ignored:") {
            return ignored::bench_ignored(&app, rel);
        }
        if let Some(query) = query.strip_prefix("early:") {
            return corpus::bench_early(&app, query);
        }
        if query == "prefs" {
            return bench_prefs(&app);
        }
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

/// `ACCENT_BENCH_SWITCHER=prefs` presents Preferences three times and prints, for each, how long
/// the action held the main loop (`open_ms`), how long until the window painted after it
/// (`first_frame_ms`), and over the dialog's presenting animation the frames painted, the
/// longest gap between two of them (`worst_frame_ms`), the time those frames spent styling and
/// laying out (`layout_ms`, the main thread's own) and the median one spent painting
/// (`paint_ms`, which under Xvfb's cairo renderer is the whole window in software every frame).
/// Then it writes `config.toml` by hand with the dialog closed and again with it up, and prints
/// what its Column Width row shows each time: the kept dialog has to show a config that moved
/// while it was away. Last it answers Restore Defaults and prints whether the dialog is the same
/// one and what the row shows.
fn bench_prefs(app: &Rc<App>) {
    /// libadwaita's floating dialog takes 300 ms to present; a little more catches its last frame.
    const ANIMATION: Duration = Duration::from_millis(400);
    let app = app.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        let Some(clock) = app.window.frame_clock() else {
            return bench_quit(&app);
        };
        for round in 0..3 {
            // Each phase's end, by its first letter: connected after GTK's own handlers, so an
            // update's mark is where the frame's styling and layout start, a layout's where they
            // end and its painting starts, and a paint's where that ends.
            let marks = Rc::new(RefCell::new(Vec::new()));
            let mark = |phase: char| {
                let marks = marks.clone();
                move |_: &gdk::FrameClock| marks.borrow_mut().push((phase, Instant::now()))
            };
            let handlers = [
                clock.connect_update(mark('u')),
                clock.connect_layout(mark('l')),
                clock.connect_paint(mark('p')),
            ];
            let t0 = Instant::now();
            let _ = WidgetExt::activate_action(&app.window, "win.preferences", None);
            let open = ms_since(t0);
            glib::timeout_future(ANIMATION).await;
            for handler in handlers {
                clock.disconnect(handler);
            }
            let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1000.0;
            let marks = marks.take();
            let between = |from: char, to: char| -> Vec<f64> {
                marks
                    .windows(2)
                    .filter(|w| w[0].0 == from && w[1].0 == to)
                    .map(|w| ms(w[0].1, w[1].1))
                    .collect()
            };
            let paints: Vec<Instant> = marks.iter().filter(|m| m.0 == 'p').map(|m| m.1).collect();
            let first = paints.first().map_or(f64::NAN, |&t| ms(t0, t));
            let gap = paints
                .windows(2)
                .map(|w| ms(w[0], w[1]))
                .fold(0.0, f64::max);
            let mut painting = between('l', 'p');
            painting.sort_by(f64::total_cmp);
            println!(
                "bench prefs round={round} open_ms={open:.1} first_frame_ms={first:.1} \
                 frames={} worst_frame_ms={gap:.1} layout_ms={:.1} paint_ms={:.1}",
                paints.len(),
                between('u', 'l').iter().sum::<f64>(),
                painting
                    .get(painting.len() / 2)
                    .copied()
                    .unwrap_or(f64::NAN),
            );
            if let Some(dialog) = app.window.visible_dialog() {
                dialog.force_close();
            }
            glib::timeout_future(Duration::from_millis(800)).await;
        }
        let column_width = |app: &Rc<App>| {
            app.window
                .visible_dialog()
                .and_then(|d| {
                    find_widget(d.upcast_ref(), &|w| {
                        w.downcast_ref::<adw::SpinRow>()
                            .is_some_and(|row| row.title() == "Column Width")
                    })
                })
                .and_downcast::<adw::SpinRow>()
                .map_or(0.0, |row| row.value())
        };
        for (case, width) in [("closed", 70), ("open", 80)] {
            let mut edited = app.config.borrow().clone();
            edited.column_width = width;
            if let Err(e) = edited.write(&accent_core::config::config_path()) {
                eprintln!("writing config.toml: {e:#}");
            }
            // The watcher's news, and its taking the file in.
            glib::timeout_future(Duration::from_millis(1500)).await;
            if case == "closed" {
                let _ = WidgetExt::activate_action(&app.window, "win.preferences", None);
                glib::timeout_future(ANIMATION).await;
            }
            println!(
                "bench prefs_edited {case} wrote={width} shows={}",
                column_width(&app)
            );
        }
        // Restore Defaults, answered: a page of defaults in the same dialog.
        let before = app.window.visible_dialog();
        let restore = before.as_ref().and_then(|d| {
            find_widget(d.upcast_ref(), &|w| {
                w.downcast_ref::<adw::ButtonRow>()
                    .is_some_and(|row| row.title() == "Restore Defaults")
            })
        });
        if let Some(row) = restore {
            row.emit_by_name::<()>("activated", &[]);
        }
        glib::timeout_future(Duration::from_millis(500)).await;
        if let Some(alert) = app
            .window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>()
        {
            alert.emit_by_name::<()>("response", &[&crate::dialogs::CONFIRM]);
            alert.close();
        }
        glib::timeout_future(ANIMATION).await;
        println!(
            "bench prefs_restored same_dialog={} shows={}",
            app.window.visible_dialog() == before,
            column_width(&app)
        );
        bench_quit(&app);
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
            crate::palette::Item::Ignored(rel) => println!("bench switcher_row ignored {rel}"),
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

/// End the run with status 2 unless the vault is a scratch copy under `/tmp`, as `make vault
/// VAULT=/tmp/<name>` makes one. For the drills that type into a note: an autosave, or the save a
/// quit makes, writes what they typed, and `testvault/` or a real vault must never get it.
fn scratch_only(app: &Rc<App>, drill: &str) {
    let root = app.root();
    if !root.starts_with("/tmp") {
        eprintln!(
            "{drill} types into its note: run it on a scratch vault under /tmp, not {root:?}"
        );
        std::process::exit(2);
    }
}

/// A drill that makes a repository in the vault root, or stages and commits in the one there,
/// refuses a vault inside a repository whose top it is not — a scratch copy under this checkout's
/// `target/` — where its `git add` and the Git pane's Stage would reach that repository's index,
/// the pane having picked it before any `git init` of the drill's. A vault in no repository is left
/// to the drill, whose own `git init` makes it one; a remote vault's git is its host's.
fn own_repository_only(app: &Rc<App>, drill: &str) {
    if app.vault().is_none_or(|vault| vault.is_remote()) {
        return;
    }
    let root = app.root();
    let top = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()));
    if let Some(top) = top.filter(|top| top.canonicalize().ok() != root.canonicalize().ok()) {
        eprintln!(
            "{drill} writes to the vault's repository: {root:?} is inside {top:?}; \
             run `git init` in it first, or use a vault outside any repository"
        );
        std::process::exit(2);
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
