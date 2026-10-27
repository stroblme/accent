//! Headless `ACCENT_BENCH_*` drills under Xvfb, printing timings or state before quitting.
//! [`install_bench_hooks`] catalogs entry points; area modules share helpers here.

use super::*;

mod answer;
mod attach;
mod chrome;
mod compare;
mod complete;
mod corpus;
mod debug;
mod diagnostics;
mod diagram;
mod dismiss;
mod export;
mod files;
mod find;
mod folders;
mod git;
mod goals;
mod goto;
mod ignored;
mod image;
mod info;
mod kernel;
mod keys;
mod loose;
mod memory;
mod minimap;
mod notebook;
mod notebook_lsp;
mod outline;
mod panes;
mod pdf;
mod present;
mod preview;
mod quickfix;
mod replace;
mod scroll;
mod search;
mod shots;
mod start;
mod style;
mod suggest;
mod symbols;
mod synctex;
mod tags;
mod toasts;

pub(crate) use answer::{SLOW_RELOAD_MS, SLOW_WRITE_MS, hold};
use answer::{bench_answer, bench_answer_doc, bench_answer_same};
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
    bench_carry, bench_clip, bench_clip_outside, bench_clip_stop, bench_close, bench_hidden,
    bench_paths, bench_save_as, bench_templates, bench_transfer,
};
use find::bench_find;
use folders::{
    bench_drag_out, bench_drop, bench_expand, bench_menu, bench_menu_press, bench_move,
    bench_unfold, bench_watch,
};
use git::{
    bench_git, bench_git_branch, bench_git_close, bench_git_commit_focus, bench_git_focus,
    bench_git_init, bench_git_markers, bench_git_press, bench_git_rebase, bench_git_scroll,
    bench_git_switch, bench_git_sync_all, bench_git_sync_over_fetch,
};
use goals::bench_goals;
use image::{bench_image, bench_image_look, bench_preview_look};
use info::bench_info;
use keys::{
    bench_box_drag, bench_hold, bench_keys, bench_list, bench_menu_caret, bench_occurrence_keys,
    bench_shell_keys, bench_term,
};
use minimap::bench_minimap;
use notebook::bench_notebook;
use outline::bench_outline;
use panes::{
    bench_apart, bench_back, bench_collapse, bench_cycle, bench_layout, bench_layout_pick,
    bench_panes, bench_pin, bench_pin_window, bench_pins_restored, bench_reload, bench_tabs,
    bench_tree,
};
use pdf::{
    bench_drawing, bench_pdf, bench_pdf_bookmarks, bench_pdf_broken, bench_pdf_closed,
    bench_pdf_comments, bench_pdf_deep, bench_pdf_deleted, bench_pdf_dropped, bench_pdf_failed,
    bench_pdf_insert, bench_pdf_pages, bench_pdf_renaming, bench_pdf_render, bench_pdf_stale,
    bench_pdf_strip, bench_sketch,
};
use preview::bench_preview_type;
use replace::bench_replace;
use scroll::bench_scroll;
use search::bench_search;
pub(crate) use start::bench_start;
use style::{
    bench_drag_fold, bench_follow, bench_listing, bench_numbers, bench_occurrences, bench_reveal,
    bench_seam, bench_style, bench_theme, bench_typing, bench_wrap,
};
use tags::bench_tags;

/// Install opt-in headless drills that print timings or state, then quit.
/// Use a generated scratch vault and isolated state; drills may write files,
/// config or repositories. `STYLE`, `OCCUR`, `SUGGEST` typing modes, `MEMORY`,
/// `MOVE` and `SCROLL` require a vault under `/tmp` (`scratch_only`).
/// Repository-writing drills also require the vault to be its repository root
/// (`own_repository_only`), preventing writes to an enclosing checkout's index.
/// Run under Xvfb; `build-aux/xtest.py :N "<steps>"` supplies real input for
/// interactive modes. `RUST_LOG=accent=debug` adds query timing breakdowns.
/// Detailed cases live in each area's bench module. Values below follow the
/// named variable's `=`; paths are vault-relative unless stated otherwise.
///
/// - `ACCENT_BENCH_EXPAND=<rel_path>`: directory-expansion timing.
/// - `ACCENT_BENCH_SWITCHER=<query>`: palette timing and initial rows; `1`
///   only opens it. Modes: `dismiss:<relA>,<relB>` (XTEST outside clicks),
///   `prefs` (Preferences timing), `ignored:<rel>` (ignored-note opening),
///   `early:<query>` (corpus arrival), `symbol:<query>` (Go to Symbol),
///   `goto:<relA>,<relB>` (Go to File's keyboard, XTEST).
/// - `ACCENT_BENCH_GIT=1`: Git pane rows, controls and refreshes. Modes:
///   `press:<path>` (hold for XTEST), `init` (discover a new repository),
///   `init:command` (Initialize Git Repository, local or remote),
///   `close:<pull|push|fetch>` (close during work), `sync` (sync during fetch),
///   `focus` (row keyboard focus), `scroll` (stable scrolled rows), `commit`
///   (post-commit focus), `switch` (repository-switch races), `markers:<rel>`
///   (conflict resolution), `branch` (branch popover, branch-name validation),
///   `syncall`.
/// - `ACCENT_BENCH_START=1`: start screen on a `--new-window` launch,
///   handled by `start::bench_start` without an `App`.
/// - `ACCENT_BENCH_KEYS=1`: editor key semantics and completion acceptance.
///   `<rel_note>` checks Return/Tab on list lines; `occur:<rel>` checks
///   occurrence shortcuts; `box:<rel>` checks Shift+Alt pointer selections.
/// - `ACCENT_BENCH_SUGGEST=escape:<rel>`: Escape with popup/ghost/find/compare
///   layers; `ghost:<rel>`: ghost-session progress and preference lifecycle;
///   `words:<rel>`: word-suggestion toggles and XTEST popup activation.
///   Typing modes require a scratch vault under `/tmp`.
/// - `ACCENT_BENCH_COMPLETE=note:<rel>`: note link/tag completion;
///   `code:<rel>`: member completion with an installed language server.
/// - `ACCENT_BENCH_CHROME=1`: focus fades. Modes: `<relA>,<relB>` (split-pane
///   fades/timing), `keys:<note>,<pdf>` (navigation), `find:<a>,<b>,<c>`
///   (active-pane/find focus), `present:<note>,<pdf>,<image>,<side>`
///   (presentation and restored layout), `toasts` (toast replacement/counts).
/// - `ACCENT_BENCH_PATHS=1`: path-entry completion and sizing.
/// - `ACCENT_BENCH_STYLE=<rel_path>`: styling latency, tag transfer and edits.
///   Modes: `typing:<rel>` (4–256 KB typing), `wrap:<rel>,<rel>…` (wrap indent,
///   including 10k code lines in the last file), `dragfold:<rel>` (XTEST folded
///   selection drag), `seam:<rel>` (hidden-line layout), `listing:<rel>`
///   (LaTeX listing colours). Every mode requires a scratch vault under `/tmp`.
/// - `ACCENT_BENCH_PANES=<relA>,<relB>`: tab moves, a reveal across panes and divider
///   stepping; `shells:<rel>`: two shells in split panes ending, one with the keyboard;
///   `bar:<a>,<b>,<c>`: a closed pane's keyboard, on its find bar or in its last tab;
///   `chords:<rel>`: the editing chords in the note, the tree and the find bar (XTEST);
///   `click:<first>,<rel>,…`: each `rel` split off beside `first` and clicked into, which has
///   to activate its pane with the keyboard; a ring inside its pane (XTEST; `shell`, `diff`).
/// - `ACCENT_BENCH_COMPARE=<rel_path>`: disk comparison alignment/focus.
///   Modes: `pads:<rel_path>` (paragraph padding), `lines:<rel_path>`
///   (line staging), `row:<repo_rel>`, `row:stale:<repo_rel>`,
///   `row:staged:<repo_rel>`, `row:commit:<repo_rel>` (Changes/Staged/history
///   rows), `pick:<repo_rel>` (repository-pick race), `clicks` (row activation),
///   `stale:<rel>` (buffer/disk mismatch), `diag:<rel_text_file>` (hidden
///   diagnostics), `gutter:<rel_text_file>` (hold for screenshot),
///   `conflict:<rel_text_file>` (sync banner), `left:<rel>` (leave comparison),
///   `folds:<rel>` (fold/run overlap), `gap:<rel>` (expand runs), `page:<rel>`
///   (zoom/indent), `press:<rel>` (XTEST hunk actions), `reader:<rel>` (reader
///   input before first hunk), `merge:<rel>` (XTEST conflict actions),
///   `unfold:<rel>` (unchanged lines/divider), `runaway:<rel>` (repeated padding),
///   `typing:<rel>` or `typing:short:<rel>` (XTEST edits; short is under 16 KB),
///   `session:save:<rel>,<other>` then `session:back:…` (restart restoration
///   using one scratch home), `large:<rel>` (main-loop timing).
/// - `ACCENT_BENCH_MEMORY=<note>,<code>,<pdf>[,<rounds>]`: object/process and
///   resident-memory retention across closes. Requires `/tmp` scratch vault.
/// - `ACCENT_BENCH_WEBIDLE=<note>`: WebKit reuse and two-second idle release.
///   Writes two diagrams; use a scratch vault.
/// - `ACCENT_BENCH_IMAGE=<rel_png>,<rel_other_png>`: zoom and replaced-image
///   reload; `zoom:<rel_svg>,…`: SVG zoom sources.
/// - `ACCENT_BENCH_IMAGE_LOOK=<rel>,<rel>,…`: image classification/recolouring
///   across themes and inversion.
/// - `ACCENT_BENCH_PREVIEW_LOOK=<rel_note>`: preview image themes and requests.
/// - `ACCENT_BENCH_TERM=1`: shell titles and saved terminal sessions.
///   `--terminal` uses a vault-less window; a vault launch opens a shell tab.
///   Use scratch `TMPDIR` so it cannot join the user's running holder.
/// - `ACCENT_BENCH_SHELL_KEYS=1`: `Ctrl+S` in a shell without window focus.
/// - `ACCENT_BENCH_HOLD=open` then `back`: shell persistence and close, using
///   one scratch state; `early`: shells closed before startup; `behind` then
///   `named`: a restored shell's tab behind another titled before it is shown.
/// - `ACCENT_BENCH_PDF=<rel_path>`: fit, save, rename and external reload.
///   Writes and moves the file; use a scratch multipage PDF. Modes:
///   `stale:<rel_path>` (remote conflicts), `failed:<rel_path>` (read-only
///   host/retry), `dropped:<rel_path>` (disconnect/reconnect and note relinking),
///   `renaming:<rel_path>` (rename during upload; remote PDF of several MB),
///   `closed:<rel_path>` (close during save; remote: while refused, then while
///   the link is down), `deleted:<rel_path>,<replacement>` (remote: deleted on
///   the host with a refused page, then replaced), `pages:<rel_path>` (page edits,
///   undo/redo and note links; generated `Attachments/pages.pdf`),
///   `insert:<rel_path>,<source>` (drop insertion; put scratch copies of
///   generated `Attachments/pages.pdf` and `Attachments/comments.pdf` at root),
///   `strip:<rel_path>` (40 s XTEST hover/drag),
///   `bookmarks:<rel_path>[,<rel_diagram>]` (outline following),
///   `render:<rel_path>` (tiles/thumbnail bursts; hundreds of heavy pages),
///   `broken:<rel_path or absolute path>` (truncation/recovery; scratch file),
///   `deep:<rel_path>[,<percent>]` (800% default; a few text pages),
///   `synctex:<rel_path>` (source/PDF round trip; scratch LaTeX `-synctex=1`
///   build, or XTEST missing-data menu probe without SyncTeX),
///   `comments:<rel_path>` (tooltips, pinning, highlights and comment outline;
///   scratch generated `Attachments/comments.pdf`),
///   `walk:<rel_path>` (comment-list loading/cancellation; hundreds of pages),
///   `pen:<rel_path>[,stop]` (main-loop gaps under 24 XTEST pen strokes while the PDF text
///   pass runs, or stopped; writes the file).
/// - `ACCENT_BENCH_DRAWING=1`: New Drawing; `sketch:<rel_note>`: Insert Sketch.
/// - `ACCENT_BENCH_TABS=<rel_note>,<rel_pdf>`: preview/tab close transitions.
///   Modes: `pin:<a>,<b>,<c>,<d>` then `pins` (save/restore one scratch session),
///   `reload:<key>,…` (edits/size after Reload Window; vault, loose file,
///   `--terminal` or `terminal://<name>`), `tree:<a>,<b>,<c>` (XTEST tree
///   clicks/drag; root must list all three), `edited:<pdf>,<diagram>,<note>`
///   (edits keep previews), `pinwin:<a>,<b>,<c>` (move to loose-file window),
///   `apart:<rel>,…` (new-window actions; creates/removes `apart-drill.md`),
///   `cycle:[<w>x<h>:]<a>,<b>,…` (25 s held Ctrl+Tab probe after XTEST `move 700 400; focus`,
///   the last of three or more split off to the right),
///   `back:<a>,<b>` (navigation across edits/reload), `loose:<rel_pdf>`
///   (loose-file readers/assets; creates/removes `loose-drill/`). Remote
///   pin-window/apart modes verify tabs cannot move off the host.
/// - `ACCENT_BENCH_SLOW_READ=<ms>`: delay text reads on workers to reproduce
///   double-clicks before tabs arrive in the `TABS=tree:` drill.
/// - `ACCENT_BENCH_FOLLOW=<rel_note>`: Ctrl-hover, missing targets and anchors;
///   `hover:<rel_note>`: XTEST hover size and wheel behavior; `click:<rel_note>`: an XTEST
///   Ctrl+click jump within the note leaves nothing selected.
///   Any text file can exercise URL underlines.
/// - `ACCENT_BENCH_OUTLINE=<rel_note>,<rel_other>`: caret following and edits;
///   `hold:<rel_note>`: 20 s XTEST probe; `index:<rel_code>`: indexed symbols.
/// - `ACCENT_BENCH_OCCUR=<rel_note>`: occurrence tags, colours and priorities.
///   Requires a scratch vault under `/tmp`.
/// - `ACCENT_BENCH_NUMBERS=<rel_note>,<rel_code>,<rel_note>`: line-number
///   preferences, hidden tabs and external config changes.
/// - `ACCENT_BENCH_THEME=<rel_note>`: theme changes and immediate tag colours.
///   Repeat with `ADW_DEBUG_COLOR_SCHEME=prefer-dark` for dark Solarized.
/// - `ACCENT_BENCH_REVEAL=<rel_note>`: temporary search/tag/line reveal.
/// - `ACCENT_BENCH_CLOSE=1`: cancelled dialogs and vault references after quit;
///   exits 1 if anything survives.
/// - `ACCENT_BENCH_CLIP=<rel_file>`: copy/cut, multi-file paste and link updates;
///   waits up to a minute per paste. `outside:<dir>`: external folder/file
///   paste, with empty `<dir>` meaning root. `stop:<dir>`: an external
///   paste stopped from the status bar.
/// - `ACCENT_BENCH_TRANSFER=<rel_pdf>`: remote open/download/upload byte counts.
/// - `ACCENT_BENCH_CARRY=<local vault>`: on a remote vault, rows carried to and from a second
///   window on the local vault, as another window's tree drop hands them over.
/// - `ACCENT_BENCH_ATTACH=<rel_note>,<rel_vault_png>,<rel_code>`: image paste/drop.
/// - `ACCENT_BENCH_ANSWER=<rel_note>`: conflict responses and main-loop timing;
///   `same:<rel_note>`: etag-only change during typing; `doc:<rel_ipynb>,<rel_drawio>`: a
///   notebook's and a diagram's reloads on a slow host (`answer::bench_answer_doc`).
/// - `ACCENT_BENCH_EXPORT=pdf:<rel_pdf>`: annotated export and print copy;
///   `note:<rel_note>`: PDF/HTML export and print dialog. Outputs use `$TMPDIR`.
/// - `ACCENT_BENCH_SAVE_AS=<rel_file>`: note/diagram/PDF/image Save As and errors.
/// - `ACCENT_BENCH_MENU=<rel_file>`: tree context menus and modifier selections;
///   `press:<rel_file>`: hold for XTEST Ctrl/Shift clicks, for example
///   `build-aux/xtest.py :99 "move X Y; keydown ctrl; down; up; keyup ctrl"`.
/// - `ACCENT_BENCH_TAGS=<rel_note>`: tag-list updates after edits.
/// - `ACCENT_BENCH_INFO=<rel>[,<rel>…]`: per-file Info sections; `terminal`
///   opens a shell instead.
/// - `ACCENT_BENCH_GOALS=<rel>:<line>:<col>[:<line>:<col>]`: Lean goals,
///   sidebar restoration and checking marks. Requires elan's Lean; excluded
///   from `make drills`.
/// - `ACCENT_BENCH_DEBUG=<rel>:<line>`: a debugpy session from a breakpoint on
///   `<line>` of the program `make drills` writes (`debug::bench_debug`).
/// - `ACCENT_BENCH_KERNEL=<rel>`: a Jupyter notebook it writes at `<rel>`, run on an ipykernel
///   (`kernel::bench_kernel`). Saves: a scratch vault under `/tmp`. `ACCENT_BENCH_KERNEL_SHOT=<png>`
///   adds a picture of the window while a cell's last outputs wait, dimmed, for its new ones.
/// - `ACCENT_BENCH_REPLACE=1`: Search-pane Replace All updates.
/// - `ACCENT_BENCH_FIND=<rel_note>`: query selection/history; `options`:
///   search toggles, invalid patterns and replacement captures in a temporary note.
/// - `ACCENT_BENCH_DIAG=<rel_code_file>`: diagnostic expansion and line-end
///   truncation. Writes the file; use a scratch vault.
/// - `ACCENT_BENCH_QUICKFIX=<rel_c_file>,<ipynb>`: Quick Fix's menu and each kind of code
///   action, against `fake-lsp.py --actions`, and a note's Create note fix
///   (`quickfix::bench_quickfix`). Writes files; a scratch vault.
/// - `ACCENT_BENCH_SEARCH=<query>[:<n>]`: result refresh after external writes;
///   `type:<query>`: ranked timing; `walk:<query>`: All/unindexed timing;
///   `more:<query>[:<n>]`: expanded per-file results; `click:`: wait for XTEST;
///   `label:<query>` or `label:<query>:xml`: diagram-label navigation; `pdf:<word>`: a PDF's
///   page row, opened on its page with the match marked (writes a PDF; a scratch vault);
///   `seed:<rel_note>`: XTEST search/replace shortcuts and box focus.
/// - `ACCENT_BENCH_HIDDEN=1`: hidden-file visibility and git row colours.
/// - `ACCENT_BENCH_UNFOLD=<rel>,<rel>,…`: folder expansion; `race:<dir>`,
///   `renew:<dir>`, `again:<dir>` and `tab:<rel>`: external unindexed changes;
///   `down:<rel>`: a remote tab's file rewritten while the link is down.
///   Changing modes write files; use a scratch vault.
/// - `ACCENT_BENCH_MOVE=<rel>,<rel>,…`: Move to… dialogs and refusal cases.
///   Requires a scratch vault under `/tmp`.
/// - `ACCENT_BENCH_DROP=out:<rel_file>,<rel_folder>`: what a row's drag offers other
///   applications, and XTEST drags of it the tree never takes as an import; moves the file.
/// - `ACCENT_BENCH_SCROLL=<rel_dir>`: list scroll retention during vault/git
///   changes. Creates a repository; requires a scratch vault under `/tmp`.
/// - `ACCENT_BENCH_DIAGRAM=<rel>`: diagram edits/save, creating a sample if
///   absent; `shot:<rel>:<dir>`: render pages; `export:<rel>:<dir>`: dark-theme
///   PDF/PNG/SVG export and print output. `ACCENT_BENCH_DIAGRAM_SHOT=<png>` adds a picture
///   of the window (`diagram::bench_diagram`).
/// - `ACCENT_BENCH_NOTEBOOK=<ipynb>,<marimo .py>,<long ipynb>`: outputs, cell edits with undo
///   and save, marimo kept as written, a long notebook's open time and memory, presentation
///   (`notebook::bench_notebook`). Saves: a scratch vault under `/tmp`. `look:<ipynb>:<dir>`:
///   pictures of it under Light and Dark at two zooms, the add strip shown, into `<dir>`.
/// - `ACCENT_BENCH_NOTEBOOK_LSP=<ipynb>,<marimo .py>`: notebooks it writes, their code cells
///   answered by `build-aux/fake-lsp.py --notebook` (`notebook_lsp::bench_notebook_lsp`). Saves:
///   a scratch vault under `/tmp`.
/// - `ACCENT_BENCH_BIBLIOGRAPHY=1`: source-preserving edits, undo, manual transfer and sidebar visibility.
/// - `ACCENT_BENCH_COLLAPSE=1`: responsive sidebar width and saved state.
/// - `ACCENT_BENCH_LAYOUT=<a>,<b>,<c>,<d>[,shell]`: save split layout and
///   active tab; `1`: inspect restored layout without saving; `pick:<rel>`,
///   `focus:<rel>`, `open:<rel>`: tab selection/focus/open during restore;
///   `quit`: quit during remote startup. Reuse scratch state for restoration.
/// - `ACCENT_BENCH_MINIMAP=<round>:<rel>`: minimap probes (`minimap::bench_minimap`).
/// - `ACCENT_BENCH_SHOT=<n>:<dir>`: README screenshot `n`, light and dark, into `<dir>`. Only
///   on the vault and launch `build-aux/screenshots.sh` makes (`make screenshots`).
pub fn install_bench_hooks(app: &Rc<App>) {
    if std::env::var_os("ACCENT_BENCH_BIBLIOGRAPHY").is_some() {
        scratch_only(app, "ACCENT_BENCH_BIBLIOGRAPHY");
        crate::bibliography::bench::run(app);
        return;
    }
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
    let notebook = std::env::var("ACCENT_BENCH_NOTEBOOK").ok();
    let notebook_lsp = std::env::var("ACCENT_BENCH_NOTEBOOK_LSP").ok();
    let clip = std::env::var("ACCENT_BENCH_CLIP").ok();
    let transfer = std::env::var("ACCENT_BENCH_TRANSFER").ok();
    let carry = std::env::var("ACCENT_BENCH_CARRY").ok();
    let menu = std::env::var("ACCENT_BENCH_MENU").ok();
    let drop = std::env::var("ACCENT_BENCH_DROP").ok();
    let watch = std::env::var("ACCENT_BENCH_WATCH").ok();
    let tags = std::env::var("ACCENT_BENCH_TAGS").ok();
    let replace = std::env::var("ACCENT_BENCH_REPLACE").is_ok();
    let info = std::env::var("ACCENT_BENCH_INFO").ok();
    let goals = std::env::var("ACCENT_BENCH_GOALS").ok();
    let debug = std::env::var("ACCENT_BENCH_DEBUG").ok();
    let kernel = std::env::var("ACCENT_BENCH_KERNEL").ok();
    let search = std::env::var("ACCENT_BENCH_SEARCH").ok();
    let find = std::env::var("ACCENT_BENCH_FIND").ok();
    let diag = std::env::var("ACCENT_BENCH_DIAG").ok();
    let quickfix = std::env::var("ACCENT_BENCH_QUICKFIX").ok();
    let save_as = std::env::var("ACCENT_BENCH_SAVE_AS").ok();
    let attach = std::env::var("ACCENT_BENCH_ATTACH").ok();
    let answer = std::env::var("ACCENT_BENCH_ANSWER").ok();
    let export = std::env::var("ACCENT_BENCH_EXPORT").ok();
    let scroll = std::env::var("ACCENT_BENCH_SCROLL").ok();
    let moving = std::env::var("ACCENT_BENCH_MOVE").ok();
    let unfold = std::env::var("ACCENT_BENCH_UNFOLD").ok();
    let shot = std::env::var("ACCENT_BENCH_SHOT").ok();
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
        && quickfix.is_none()
        && tags.is_none()
        && !replace
        && info.is_none()
        && goals.is_none()
        && debug.is_none()
        && kernel.is_none()
        && search.is_none()
        && clip.is_none()
        && transfer.is_none()
        && carry.is_none()
        && menu.is_none()
        && drop.is_none()
        && watch.is_none()
        && diagram.is_none()
        && notebook.is_none()
        && notebook_lsp.is_none()
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
        && shot.is_none()
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
        if let Some(arg) = shot {
            return shots::bench_shot(&app, &arg);
        }
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
            if let Some(arg) = rel.strip_prefix("deleted:") {
                return bench_pdf_deleted(&app, arg);
            }
            if let Some(rel) = rel.strip_prefix("comments:") {
                return bench_pdf_comments(&app, rel);
            }
            if let Some(rel) = rel.strip_prefix("walk:") {
                return pdf::bench_pdf_walk(&app, rel);
            }
            if let Some(arg) = rel.strip_prefix("pen:") {
                return pdf::bench_pdf_pen(&app, arg);
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
        if let Some(local) = carry {
            return bench_carry(&app, &local);
        }
        if let Some(rel) = clip {
            if let Some(dir) = rel.strip_prefix("outside:") {
                return bench_clip_outside(&app, dir);
            }
            if let Some(dir) = rel.strip_prefix("stop:") {
                return bench_clip_stop(&app, dir);
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
        if let Some(rels) = answer.as_deref().and_then(|a| a.strip_prefix("doc:")) {
            return bench_answer_doc(&app, rels);
        }
        if let Some(rel) = answer {
            return bench_answer(&app, &rel);
        }
        if let Some(rels) = info {
            return bench_info(&app, &rels);
        }
        if let Some(arg) = goals {
            return bench_goals(&app, &arg);
        }
        if let Some(arg) = debug {
            return debug::bench_debug(&app, &arg);
        }
        if let Some(rel) = kernel {
            return kernel::bench_kernel(&app, &rel);
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
        if let Some(rel) = quickfix {
            return quickfix::bench_quickfix(&app, &rel);
        }
        if let Some(rel) = menu {
            if let Some(rel) = rel.strip_prefix("press:") {
                return bench_menu_press(&app, rel);
            }
            return bench_menu(&app, &rel);
        }
        if let Some(arg) = drop {
            if let Some(arg) = arg.strip_prefix("out:") {
                return bench_drag_out(&app, arg);
            }
            return bench_drop(&app, &arg);
        }
        if let Some(arg) = watch {
            return bench_watch(&app, &arg);
        }
        if let Some(arg) = diagram {
            return bench_diagram(&app, &arg);
        }
        if let Some(arg) = notebook {
            return bench_notebook(&app, &arg);
        }
        if let Some(arg) = notebook_lsp {
            return notebook_lsp::bench_notebook_lsp(&app, &arg);
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
                (None, "init") => bench_git_init(&app, false),
                (None, "init:command") => bench_git_init(&app, true),
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
        if let Some(rels) = query.strip_prefix("goto:") {
            return goto::bench_goto(&app, rels);
        }
        if let Some(query) = query.strip_prefix("early:") {
            return corpus::bench_early(&app, query);
        }
        if let Some(query) = query.strip_prefix("symbol:") {
            return symbols::bench_symbol(&app, query);
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

/// Time three Preferences presentations: `open_ms`, `first_frame_ms`, frame count,
/// `worst_frame_ms`, total `layout_ms` and median `paint_ms`. Cairo paints the
/// whole window in software under Xvfb. Verify external config changes update
/// the retained dialog while closed or open, and Restore Defaults reuses it.
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

/// Find the first `GtkSearchEntry` for direct headless input. Pass the palette
/// dialog: searching from the window finds the sidebar entry first.
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

/// Read visible error-toast text through widgets because libadwaita has no
/// overlay query API.
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

/// Emit an entry-controller key press without a window manager or external input tool.
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

/// Dispatch pending main-loop work so idle find highlighting reaches the buffer.
fn bench_pump() {
    let context = glib::MainContext::default();
    for _ in 0..10_000 {
        if !context.iteration(false) {
            return;
        }
    }
}

/// Exit 2 unless the vault is under `/tmp` (`make vault VAULT=/tmp/<name>`).
/// Typing drills autosave, so they must never use `testvault/` or real notes.
fn scratch_only(app: &Rc<App>, drill: &str) {
    let root = app.root();
    if !root.starts_with("/tmp") {
        eprintln!(
            "{drill} types into its note: run it on a scratch vault under /tmp, not {root:?}"
        );
        std::process::exit(2);
    }
}

/// Reject local vaults nested in another repository, preventing drill staging
/// from touching its index. Allow a vault that is its repository root or has
/// no repository yet. Remote git runs on the host and is not checked here.
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
