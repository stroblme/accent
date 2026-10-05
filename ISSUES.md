# Dependency issues

Bugs and limitations in the libraries and tools accent is built on, each of which cost us a workaround, a skipped feature or a deferral. An entry says what goes wrong and where it was seen, where it stands upstream, what we do about it and where that lives, and what would let the workaround go. Open work on our side stays in NOTEPAD.md; "(NOTEPAD)" points at the item there. Nothing here has been reported upstream unless the entry says so, and "worth a report" marks the ones that should be. An entry marked **Floor** can go once the version it names is the oldest we build against.

Versions in play (2026-10-05): GTK 4.22.5, libadwaita 1.9.4, GtkSourceView 5.20.0, WebKitGTK 2.52.6 and VTE 0.84.1 on the development machine; the build's floors are GTK 4.18, libadwaita 1.7, GtkSourceView 5.18 and libvte 0.78 (`apps/gtk/Cargo.toml`), and the Flatpak runs the GNOME 50 runtime.

## GTK 4

- **Iter lookups abort over hidden text.** `gtk_text_layout_get_iter_at_position` hands a line's whole byte count to `gtk_text_iter_set_visible_line_index`, which counts visible bytes only, so a lookup on a line holding hidden text, or on hidden lines GTK has not measured again yet, walks on into the lines after and aborts with "Byte index … is off the end of the line" (GTK 4.22; three aborts of the installed app between 2026-09-29 and 10-01, two of them from GtkSourceView's gutter and annotations, which ask at the screen's top and bottom rows every frame). Upstream: worth a report. Workaround: hidden text keeps to whole lines (`fold::whole_lines`, 4238aacc), a comparison opens the tab's folds while it is up (5788f961), our pointer lookups go through `fold::iter_at_location` (1896d1b6), and `multicaret::View::snapshot` skips a frame whose edge row would abort (`fold::aborts_at`, d5390741, f02800fd). GTK's own press and drag gestures, GtkSourceView's hover, the gutter's prelit row and a shown minimap still ask unguarded (NOTEPAD). Goes when GTK counts visible bytes there.
- **The overlay scrollbar's fade handler outlives its adjustment.** GTK 4.22 connects it to the scroller's adjustment at every realize and takes it off only while realized, so a scroller handed another adjustment outside its window left a handler that later ran on freed memory: a SIGSEGV in `gtk_widget_get_mapped` that took every window down (2026-09-28), and a `GTK_IS_WIDGET` assertion. Upstream: worth a report. Workaround: `diff::swap_vadjustment` turns overlay scrolling off and on around the swap, and `Tab::compare` turns it off before the document leaves the window (f461bd80, 488f4bde).
- **A freed GtkTextView stays connected to its adjustment.** A comparison column dropped while it shared the editor's adjustment left a dangling handler, and the next wheel turn crashed every window (SIGSEGV in `gtk_text_view_value_changed`, 2026-09-25). Upstream: worth a report. Workaround: `Compare::leave` gives the column its own adjustment back (15e34196).
- **A text view's snapshot reconfigures its adjustment after layout.** In a comparison GtkTextView's snapshot calls `gtk_adjustment_configure` on the adjustment both columns share after the scrollbars were allocated, so GTK logs "Trying to snapshot GtkGizmo … without a current allocation" for the editor column's trough in about half the runs of `ACCENT_BENCH_COMPARE=typing:short:` (GTK 4.22). Workaround: none; the ways out (an adjustment per column, or no scrollbar on the editor's column) change the comparison's design (NOTEPAD).
- **Lines keep a stale height, and are laid out a chunk at a time.** A line keeps the height it was last laid out at until GTK lays it out again, an unvalidated one reports 0, and the lines above a view are laid out a chunk of pixels at a time. Workaround: a comparison measures a just-edited line in its own layout until GTK agrees (`diff::pad::UNMEASURED`, 2a88eca7); its second column still trails the editor's by up to a few hundred pixels for 1–3 frames after Show All Unchanged Lines, since anchoring both needs them laid out alike, which GTK has no call for (NOTEPAD).
- **GtkTextView's own selection drag carries only what shows.** A move of a selection holding folded lines deleted the hidden lines it did not carry, and the view's drop target answers any drag it did not start with a copy. Workaround: the drag is always ours (`editor/drag.rs`, mirroring GTK's private hit tests; e033f958, e7a9b276).
- **Copy and paste carry tags.** Cut and copy put a tagged `GtkTextBuffer` on the clipboard, and a middle-click paste of the primary selection brought the note's tags along, a fold's included. Workaround: plain-text clipboard handlers and a primary paste read as text (`editor/lines.rs::line_clipboard`, `primary_paste`; 7217c403, a94e9296).
- **`gtk_text_iter_forward_visible_line` steps through hidden text a character at a time.** Up and Down over a 20 000-line fold take 55 ms and 29 ms (release, Xvfb, 2026-10-03). Workaround: none yet; jumping the hidden run by its tag toggle is the fix if a large fold drags (NOTEPAD).
- **No call for a fresh pointer pick.** GTK aims a press at the widget the last motion was over, so in presentation a press on the status bar that slid up under a resting pointer reaches the document under it; `gdk_surface_request_motion` is private (seen on X11 through XTEST, not checked on Wayland). Workaround: none; any motion over the bar first avoids it (NOTEPAD).
- **A popover is re-presented only when its parent is allocated.** `GtkListView` allocates its children itself and never takes that path, so a context menu hung off a list froze at its first size and scrolled. Workaround: menus hang off a plain box, the pointer translated into it (`tree::build`, `fileops/menu.rs::context_menu`; e1c559fc).
- **GtkPopoverMenu emits `closed` inside the item's own `clicked`.** Unparenting the menu there takes the action muxer with it, and the action is dropped. Workaround: `widgets::popup_menu` unparents from an idle (3d22bccd, 159b85ae).
- **A list row touched inside `bind` breaks the list.** Adding a class or a handler to the row widget during the factory's bind leaves the list manager handing GTK null children, one `gtk_widget_insert_after` critical per row. Workaround: done from an idle (`git/changes.rs::bind_change`, cf34868b).
- **Wayland drag and drop.** A widget mapped mid-drag is never allocated, so it cannot be picked: the drop sheet stays mapped and toggles `can-target` instead (e6d8474f). A declined drop is cancelled (GTK destroys the offer, mutter cancels the source) rather than reaching libadwaita's `create-window`, so declining works on X11 only: the drop zones take the drop (`App::move_in`, 27e66336, checked under headless mutter 50.4).
- **Pen detection differs by backend.** On Wayland a tablet arrives as a mouse-source device, and on X11 without libwacom there is no tool at all. Workaround: `pdf/view.rs::from_stylus` asks the device tool, then the pen source, and never takes touch (14af04c4).
- Smaller gaps, each worked around in place:
  - No indeterminate `GtkProgressBar`: `widgets::Pulse` steps it on a timer, removed when the work ends.
  - `GtkEntryCompletion` is deprecated with no replacement: `pathfield::path_field` lists folders in a revealer (36c3cafe).
  - `GtkTextTag` has no opacity: focus mode's line fade is a painted veil (`fade.rs`).
  - One insert mark per buffer: multiple carets are a `sourceview5::View` subclass of ours (`multicaret/`, 53c80df5), and IME, dead keys and preedit reach the primary caret only.
  - No public way to take an overlay child off a text view (`gtk_text_view_remove` warns): a comparison's and a conflict's buttons are pooled and reused (`diff/pool.rs`, `conflict.rs`).
  - `GtkTextBuffer` ends a line at U+2029 and at a lone `\r`, where the diff does not: comparisons address text by character offset (0ac9e2c4, `diff::line_starts`).
  - Text inserted at a tag's start does not take the tag, a line's spacing comes from the tag on its first character, and a tag's `pixels-above-lines` replaces the view's rather than adding to it: a comparison's padding is laid and read back by hand (`diff/pad.rs::pad`, `reclaim`).
  - Text inserted at the start of a hidden run lands outside it, a deletion beside the caret can take a hidden character, and a caret moved to the end of the text lands hidden: `fold::resync`, `whole_lines`, `shown` (865b00f9).
  - No hanging indent for a wrapped line: `wrap.rs` lays negative-indent tags (`wrap1` … `wrap32`).
  - The text view's menu greys Cut and Copy with nothing selected: `editor/lines.rs::menu_items` enables them again from an idle (28461538).
  - Class bindings take chords of ours (GtkTextView's `Ctrl+Up` / `Down` and `Ctrl+K`, GtkSourceView's `Shift+Alt+Up` / `Down`): `actions::CAPTURED`.
  - Application accelerators dispatch in the window's capture phase, ahead of the focused widget, a terminal or an entry included: unbinding is the only way a key reaches a shell (`Shell::apply_accels`, `actions::reserved`).
  - `GtkButton` claims its press only on release, so the text view under a button laid over it saw the press too: `widgets::claim_press` (4190d0ae).
  - Gestures stop at button 3: the mouse's back and forward buttons go through an `EventControllerLegacy` (`wire::wire_window`).
  - No size-allocate signal: a tab learns its width from a zero-size `GtkDrawingArea` (`editor/open.rs::open`), a terminal waits for its first layout (`terminal::when_laid_out`).
  - Removing the focused widget while GTK moves the focus hung the app: `DiagramTab::edit_label` finishes from an idle (b89deb8e).
  - `Widget::color()` resolves only once the widget is mapped: views restyle on map.
  - Scroll events carry no pointer position: `zoom::zoom_on_wheel` tracks the pointer itself.
  - `GtkPaned` has no drag state and claims the press sequence, its handle is a private 1 px gizmo, and it splits a resize half and half: `paned::watch`, the `paned.dragging` rule, and a 2 px shift of the neighbour while dragging (NOTEPAD).
  - `GtkTreeExpander::set_list_row` runs the `TreeListModel` create-func and throws the model away again, and a `TreeListModel` splice recreates every row and collapses open folders: `tree::Tree` caches child models and splices only the span that changed (`tree/listing.rs::changed_span`).
  - Removing the focused list row sends focus to the window's first focusable widget, scrolling there: `widgets::hand_on_focus`.
  - A ScrolledWindow leaves its child's adjustments set, so strong handlers on them leaked PDF tiles and WebKit processes: `scrollable::adopt` holds them weakly.
  - A tooltip set on every bound row took a folder's expand from 12 to 40 ms: rows answer `query-tooltip` instead (e1c559fc).
  - `GtkDropTarget` matches on a GType, never a mime type: a tree row drags as a `GtkStringObject` (`tree/drag.rs`).
  - `GtkSearchEntry` waits 150 ms before `search-changed` and takes the first Escape to clear the query: `search_delay(0)`, and the find bar and the palette answer Escape first.
  - `GtkApplication::quit` skips `close-request`, where unsaved buffers are written: `shell::quit`.
  - `GtkSnapshot` blends only its own children, so the highlighter's live stroke is a shade lighter over text than its tile will be (NOTEPAD).
  - GTK 4 cannot place a window, so Reload Window opens where the compositor says; the colour chooser cannot be given a palette, so none is imitated (DESIGN.md).
  - Print to File is found by a translated name and a backend type that differs built in and as a module (4.22): `export::file_printer`.

## GDK, GSK, cairo, Pango

- **GDK's decoders ignore EXIF orientation.** A camera JPEG or a WebP shows unturned, and libtiff's oriented read mirrors a TIFF but does not make the quarter turns (orientations 5–8). Workaround: the core reads the tag (`accent_core::orientation`; ff1e68de, d28ab0b3) and `look::after_libtiff` finishes a TIFF's turn.
- **A texture is capped at 32 767 px.** A diagram page of 300 formulas typeset as one picture failed every crop, so the labels were blank, with a `gdk_texture_download_surface` warning. Workaround: batches no taller than 8192 px (`math::fitting`, bb51fef0).
- Smaller gaps:
  - GSK refuses a zero corner radius (`diagram/paint.rs::outline`).
  - A cropped texture keeps the whole one alive (`math::Typesetter::collect`).
  - Pango counts a label's `lines(2)` per paragraph, so a search snippet is collapsed into one paragraph before it is shown (`sidebar/search.rs`, 6902862c).
  - Fontconfig resolves CSS `monospace` to Liberation Mono, a face nothing else in the window uses (`highlight::monospace_family`).
  - Pango has no mathematics, so a diagram's formulas are typeset by a hidden WebKit view (`diagram/math.rs`), one web process for the app.

## libadwaita

- **A lone tab cannot be dragged.** libadwaita starts a tab drag only while the view holds more than one page (`reorder_update_cb` in `adw-tab-box.c`: `adw_tab_view_get_n_pages (self->view) > 1`), so the last tab of a pane only slides along its own bar (1.9.3; the same condition is on libadwaita's main branch, checked 2026-10-05). Upstream: not reported, by user decision (2026-09-10). Workaround: Move Tab (`Shift+Alt+arrows`) moves it and closes the pane it leaves; a `GtkDragSource` of ours on a lone tab's bar is the way out if the drag is ever wanted (NOTEPAD).
- **A tab dropped on the desktop goes back where it came from.** A window for it would have to be built inside libadwaita's drag handling (`create-window`), where `page-attached` and reorders fire while libadwaita still holds the page and re-parenting the pane fails GTK's assertion; `attach_page` is not public, so `create-window` is the only way to re-home a page at all. Workaround: a landing is recorded and applied from an idle (`Shell::adopt_soon`, `landed`, `Landing`; 878cd907), and Move to New Window (`Shell::move_apart`) does what the drop would. Deferred to a real desktop session, Xvfb having no window manager to take the drop (NOTEPAD).
- **AdwTabBar never scrolls the selected tab back into view.** It scrolls on a selection change, not on a new allocation, so a pane narrowed after its tab was picked leaves that tab half out of its bar; selecting the selected page is a no-op, and `AdwTabBox` is private with no page-to-widget lookup. Upstream: worth a report (re-scroll to the selected tab on allocate). Workaround: none (NOTEPAD).
- **Dragging a tab logs AdwFadingLabel warnings.** Every tab-bar drag logs `gtk_widget_size_allocate(): attempt to allocate AdwFadingLabel widget with width -40` and a paired GtkLabel measure warning, from libadwaita's tab-label transition. Workaround: none needed; warnings do not trip `G_DEBUG=fatal-criticals` (NOTEPAD).
- **A floating dialog's dimming closes nothing.** libadwaita closes a dialog on an outside press only as a bottom sheet; a floating one's dimming is a `GtkWindowHandle` (`adw-floating-sheet.c`), and the press never reaches the window's controllers. Workaround: `dialogs::close_on_outside_press`, a capture-phase gesture on the dialog itself, for the palette, Preferences and About (a540a8a8, 2d5adc9e, a6ecf979).
- **AdwStyleManager raises `notify::dark` before it swaps the stylesheet.** Colours read in that emission were the outgoing theme's: near-black ink on a near-black page after a switch to dark. Workaround: `App::restyle_all` runs one main-loop turn later (dd30c2ca); one frame still paints the outgoing tag colours (NOTEPAD).
- **AdwToastOverlay shows one toast and queues the rest.** A stale Undo could stand while newer news waited behind it. Workaround: a pile of our own under Adwaita's `toast` node (`toasts.rs`, c1f56ebe).
- **No shortcuts window below libadwaita 1.8.** `AdwShortcutsDialog` needs 1.8 and `GtkShortcutsWindow` is deprecated since GTK 4.18, while the floor is 1.7 (CI's desktop job runs in `fedora:44`, Ubuntu 24.04 shipping 1.5). Workaround: the palette's command mode lists every accelerator. **Floor: libadwaita 1.8.**
- Smaller gaps:
  - `AdwTabPage` takes no style class and no title markup, so a preview tab is marked by its indicator icon rather than VS Code's italics (DESIGN.md, Tabs).
  - `AdwBanner` has one button, so a tab's questions queue rather than merge (0ac9e2c4).
  - `AdwTabView` binds its chords in the capture phase, so its `Ctrl+Shift+Home` / `End` took GtkSourceView's own: `panes::shortcuts` takes six of them away (67208d3d, 2773d474).
  - `AdwTabView`'s drop target takes every drop as "move here": a drop sheet is picked first (63c7da42).
  - `AdwTabView` picks the left neighbour itself when the selected page closes (`nav::select_survivor`, 67208d3d), `close_page_finish` skips the `close-page` handler (`save::App::forget_page`), `page-detached` fires in the middle of `transfer_page` (the emptied pane closes from an idle), and `setup-menu` fires again with no page after the menu hides (`wire::wire_pane`).
  - `AdwTabBox` claims the press, so a double-click on the bar is caught by `panes::on_tab_double_click`; the tab chords reach a terminal only in the bubble phase (`terminal::install_keys`).
  - `AdwClamp` is not `GtkScrollable` and puts a viewport in between, so `scroll_to_mark` wrote to nothing: `AdwClampScrollable` (e46dc773).
  - An alert's `map_tick_cb` grabs its entry again and selects all of it: `dialogs::focus_entry`.
  - `AdwOverlaySplitView` cannot be dragged: the sidebar is a `GtkPaned` (`App::install_collapse`), which no longer overlays a narrow window.
  - The success and error colours exist only as CSS variables Rust cannot read: `diff::tint` mixes fixed hues with the foreground.
  - `AdwPreferencesPage` keeps no scroll position, so a page rebuilt by a hand edit of `config.toml` starts at its top (NOTEPAD).
  - The toolbar's bottom bar is a window handle, so a right press opened the window menu: `wire::wire_window`.
  - Stylesheet fix-ups in `install_chrome_css`: a linked `osd` button row drew black corners (2c346dc0), the 24 px button minimum (`.accent-bar-button`), a lone header's extra padding (`.accent-lone-header`) and the search bar's shade line (`.accent-flat`).

## gtk-rs bindings

- **`AlertDialogExtManual::choose` leaks the dialog.** libadwaita-rs 0.9.2 passes `self` to `adw_alert_dialog_choose` as a full reference (`self.upcast().into_glib_ptr()`) where the C side takes it `transfer none`, so every dialog outlived its close with whatever its handlers held: New File's path field kept a closed window's vault open. 0.9.2 is still the newest release (crates.io, checked 2026-10-05). Upstream: worth a report. Workaround: `dialogs::choose` presents the dialog and answers once from `connect_response`, for all 20 callers (e22bd30e). Goes when a release passes `self` borrowed; `MessageDialog::choose` has the same leak and is unused.
- Smaller gaps:
  - vte4 0.10's `v0_78` feature deprecates `current_directory_uri`, which fails clippy `-D warnings`, so `vte4` stays on `v0_76` and `vte4-sys` takes `v0_78` for `vte_install_termprop` (`apps/gtk/Cargo.toml`, 2773d474).
  - vte4 0.10 does not bind `vte_get_user_shell`: `terminal::user_shell` reads `$SHELL`.
  - gtk4-rs 0.11 exposes no `css_changed` vfunc: `Tab::rehang`.
  - sourceview5's `GutterRendererText` is not subclassable (an `unsafe impl IsSubclassable` in `editor/page.rs`), and `forward_iter_to_source_mark` is unimplemented (`diagnostics::painted` walks the lines).
  - webkit6's `WebsiteDataManager::clear` wants a `Send` callback: `preview::forget_images` wraps it in a `ThreadGuard`.

## GtkSourceView

- **The minimap logs `gtk_adjustment_set_value: assertion 'isfinite (value)' failed`.** 5.20's `update_child_vadjustment` divides by the view's `upper - page_size` unguarded, and a fresh view reports 0 there for a frame (a comparison's new column, or a tab whose first pass validated exactly one screen). Harmless in production, since the assertion returns before the value is set, but fatal under `G_DEBUG=fatal-criticals`, so drills run with the minimap off. Upstream: fixed in 5.22.0 (2026-09-18): the division is guarded and a disconnect cancels the queued tick. Workaround: none in app code (decided 2026-09-26, NOTEPAD). **Floor: GtkSourceView 5.22**, the Flatpak moving to the GNOME 51 runtime that ships it.
- **The minimap's visible-region slider paints nothing.** GtkSourceMap builds and allocates its `slider` child, coloured from the scheme's `map-overlay`, but nothing shows on GTK 4.22 with 5.20; rules on `slider` and on `textview.GtkSourceMap slider`, and a user-priority provider on the slider itself, each tried alone, tinted nothing, and a live adjustment did not change it. Upstream: not reported; 5.22.0's NEWS lists "Fix GtkSourceMap slider sizing and positioning when the map is taller than its contents", not re-tested here. Workaround: none, the minimap has no indicator; drawing the band in a `snapshot` override is the fallback (NOTEPAD).
- **The gutter keeps the view it last painted.** `GtkSourceGutterLines` holds the view, gutter and buffer until the next paint, which a view out of its window never gets, so every closed tab leaked 0.6–1.1 MB. Upstream: 5.22.0's NEWS lists "Fix gutter renderer list and gutter lines leaks"; whether that covers this is not checked. Workaround: `editor::release` disposes a view once it has left its window (522ba6a7).
- **GtkSourceMap follows the adjustment the view had when it was set.** Across an adjustment swap it stood still while the comparison scrolled, and a tab closed mid-comparison logged "instance … has no handler with id". Workaround: `Tab::with_map_unset` lets go of the view around the swap (`editor/compare.rs`).
- **An emptied assistant is presented 0 px wide.** A hover that empties while it is up is allocated 0 wide, which `gdk_popup_present` refuses with a critical (six in the installed app's journal from 2026-09-25). Workaround: assistants are never narrower than 1 px (`install_chrome_css`, 1393e245).
- **Escape over the completion popup goes on to what is behind it.** The list hides itself from its own capture controller and lets the press propagate (`key_press_propagate_cb` in `gtksourcecompletionlist.c`), so the same Escape closed the find bar or the comparison. Workaround: a capture controller on the tab's scroller stops Escape while the popup is mapped (7c83ce48).
- **Completion has no state to ask.** Nothing says whether the popup is shown, `hide` is not emitted when the view leaves the screen, there is no selected-row getter, and the list's `proposal` property raises a critical with nothing selected (5.20 reads item -1). Workaround: `Tab::popup_shown` and `editor/keys.rs` look at the mapped `GtkSourceCompletionList` child and its rows (459e4c9a, 1fee1aa1).
- **A snippet without a trailing chunk fails an assertion.** Tab past the last stop moves the caret with no chunk current, and `_gtk_source_snippet_insert_set` asserts; there is no "snippet active" state and no call to end one. Workaround: every template ends in an empty chunk 0 (`editor/lines.rs::chunks`), and `Tab::end_snippet` toggles `enable-snippets` (3057d56c).
- Smaller gaps:
  - `is_trigger` is asked about single characters only, so `[[` never opened the list: `typing::on_char` shows it (9417fae9).
  - The popup filters rows again with its own subsequence match, which dropped `[[` rows, folded accents and encoded links: the rows' filter strings are adjusted (9203b302, `fuzzy::Query::as_typed`).
  - One failing provider empties the whole popup: our `fetch` never returns an error.
  - Two `SearchContext`s on one buffer race for tag priority, and a search for `e` over a 1 MB note took 2.3–4.5 s: the occurrence highlight is a plain tag (84f630e4) and find is our own matcher (`accent_core::search`, 73f4da98).
  - The 5.20.0 `latex.lang` styles an `lstlisting` or `minted` body as verbatim, which maps to a comment: a patched copy goes first on the search path (`apps/gtk/data/language-specs/latex.lang`, d3db42d4), to diff against a newer upstream file.
  - A tab stop sits at the rounded pixel width of `tab_width` spaces, so a tab-indented line's wrap hangs up to 1 px short of its text (`wrap.rs`, NOTEPAD).
  - No folding and no multiple carets: `fold.rs` (an invisible tag and a chevron gutter, 253e827e) and `multicaret/` (53c80df5).
  - The style scheme's CSS sits at priority 598/599 and striped the gutter: application-priority rules in `install_chrome_css`.
  - An end-of-line annotation too long for the line is drawn two lines lower and clipped: `diagnostics::fit`.

## WebKitGTK

- **`WebKitFindController` reports a total, never a position.** It stops at the match ceiling it is handed (500, `FIND_LIMIT`), has no regular expressions, and its whole-word option matches word starts only (`AT_WORD_STARTS`). Workaround: the preview tracks the position itself, dropping the selection before each search, so its find restarts at the first match on every keystroke where the editor's keeps its place; Regular Expression is not offered there (`preview.rs`, `FIND_LIMIT`; 6cebb613, 6247c683). Revisit with a way to ask where the current match is (NOTEPAD).
- **The memory cache is cleared only whole.** An image changed, removed or renamed on disk stayed stale in the preview. Workaround: the whole cache goes and the note renders again (`Preview::holds`, `forget_images`; 95d0c95f, 5f1ba005), so every image on the page is served again; a cache-busting query per changed image would serve only that one (NOTEPAD).
- **A dead web process freezes the view.** After a crash, or past its memory limit, the preview keeps its last frame and follows nothing, and a batch of diagram formulas is lost. Workaround: `Preview::connect_lost` renders the note again in a new process, and `Typesetter::lost` typesets the batch again once (c85cb18c, 60efa994).
- **No TIFF, and EXIF orientation for JPEG only.** Workaround: a TIFF, or any other turned image, is served to the preview and to exports as an upright PNG (`look::serve`, `export::data_uri`; d28ab0b3, b950d866).
- **A note exported as PDF is ~2.4 MB for two pages of text.** WebKit's print path through cairo embeds Adwaita Mono and its bold nearly whole, where a subset of the glyphs used would be a few KB. Workaround: none; another face on paper or a pass through a PDF optimiser would shrink it (NOTEPAD).
- Smaller gaps:
  - WebKit cannot see GTK's CSS variables: the page background is a literal from `theme.rs`, set on the view too so a dark page does not flash white.
  - User-script errors arrive scrubbed to "Script error.": the preview relays its console through a message handler (`RUST_LOG=accent::preview=debug`, 6cebb613).
  - `decide_policy` sees navigations only: network access is blocked by a content filter, compiled asynchronously (`preview::block_network`).
  - A search before the page has loaded finds nothing: the query is issued again on load (5172a556).
  - Ctrl+scroll zooms the page by itself: the preview's zoom goes through `zoom::zoom_on_wheel`.
  - A print operation can report `failed` before `finished`, or never answer: `export::printed` settles on whichever comes first, under a timeout.

## VTE

- **No OSC 52.** VTE 0.84 lists it as unimplemented, so a program's copy (Vim over ssh, Claude Code) never reached the clipboard, and a termprop cannot carry the copy either: a value is capped at 2 KiB, a burst keeps only its last value, and a BEL-terminated sequence is ignored. Workaround: `accent-cli attach` keeps the copy beside the holder and raises the valueless `vte.ext.accent.clipboard` termprop, ended by ST, and the tab fetches the copy with `accent-cli clip` (79a97cfe). A shell with no accent-cli under it gets no clipboard.
- **The default colours do not fit the theme.** VTE's built-in palette is arithmetic (its blue is 1.41:1 on our dark background), its foreground follows the legacy `@theme_text_color`, so it never followed Solarized, `vte_terminal_set_colors` keeps alpha only on the background, and a palette not 0, 8, 16, 232 or 256 long asserts. Workaround: a palette per theme (`theme::terminal_palette`), a foreground composited beforehand, and a palette that falls back whole rather than short (`terminal::paint`; 8375d368, 6e0a47db).
- Smaller gaps:
  - VTE claims the primary press, so a link opens on `Ctrl`+click (2773d474).
  - No copy and paste chords and nothing for Select All: `win.terminal-copy` / `-paste` are ours, and Select All is left out of the menu.
  - Its scrollback is its own, so Find and Go to Line do nothing over a shell (c41506a1).
  - A VteTerminal not finalised keeps its VtePty open, and GTK 4 has no `destroy`: the terminal is captured weakly (9da3094c).
  - Termprops must be registered before the first terminal and need libvte 0.78 (`terminal::install_termprops`, 39e10a05).
  - OSC 7 does not cross ssh (`VTE_VERSION` is not forwarded), and zsh here does not emit it, so such a shell starts again where it was opened (NOTEPAD).

## pdfium and pdfium-render

- **pdfium-render 0.9.4 makes the raw bindings private.** `PdfiumLibraryBindingsAccessor` is crate-private again in 0.9.4, and the calls pdfium-render does not wrap reach pdfium through it, so 0.9.4 does not build. Upstream: part of the `thread_safe` soundness work of [pdfium-render#262](https://github.com/ajrcarey/pdfium-render/issues/262), open (checked 2026-10-05); 0.9.4 is the newest release. Workaround: pinned at `=0.9.3` (`crates/core/Cargo.toml`, 4d0af3c5). Goes when a release makes raw access public again, or those calls go through a second `PdfiumLibraryBindings` from `Pdfium::bind_to_library` under our own lock (NOTEPAD).
- **`thread_safe` does not serialise calls.** Since 0.9.0 the feature only adds `Send` and `Sync`, contrary to its README, and pdfium is not thread-safe: two threads touching it abort with `free(): invalid size`. Upstream: this is pdfium-render#262 (open). Workaround: every entry point holds one global lock (`pdf::CALLS`), so renders never overlap; a pdfium per worker process, as pdfium's authors recommend, is the upgrade.
- **The colour getters cast an annotation to a page object.** `fill_color()` and `stroke_color()` fall back to `FPDFPageObj_GetFillColor` on the annotation's handle when `FPDFAnnot_GetColor` fails, as it does on any annotation with an appearance stream: undefined behaviour that segfaults (0.9.3 against pdfium 7881 and 8035, so not an ABI mismatch). Upstream: worth a report. Workaround: `annot::annotation_color` reads the generated path's fill instead; an `/AP` holding no page objects still takes the crashing branch.
- **Ink, page moves and most shapes are not wrapped.** pdfium-render has no `FPDFAnnot_AddInkStroke`, no `/InkList`, no `FPDF_MovePages` and no line or circle constructor, and it gives an appearance stream to ink and stamps only. Workaround: a stroke's geometry lives in its appearance stream and every shape is an `/Ink` (42db32bf); the rest are raw calls through the accessor (`pdf::pages::move_page`, ae494169; `annot::add_ink_list`, 30f49e57). Another editor can redraw our strokes but not reshape them (NOTEPAD).
- **Saving rewrites the whole file.** `FPDF_SaveAsCopy` writes the whole document under the pdfium lock (80–140 ms for a 100 MB scan, 25–50 ms for 383 pages of text) and drops whatever incremental history the file had, so every ink save rewrites the PDF, stalls that document's tiles meanwhile, and hands Syncthing a whole-file change. `FPDF_INCREMENTAL` is no way out: pdfium appends every object it has parsed since the document opened, so a 104 MB scan read through before a stroke saves at 209 MB, in no less time (measured 2026-09-30). Workaround: none, the full save is accepted (`PdfDoc::save`, `pdf/render.rs`; a8fd3caf); highlights live in the notes rather than the PDF for the same reason (DESIGN.md).
- **A file rewritten in place renders blank.** pdfium reads the file as it needs it, so a PDF written into under an open tab, as pdflatex writes, is no longer the document that was opened, and pages not read yet come out blank. Workaround: `PdfDoc::intact` notices and the tab reads the file again.
- **A deleted page cannot be given back.** pdfium's one-page copy (`take_page` / `put_page`) dropped every reference to another page, so an undone delete brought the page back with dead links, bookmarks and `\ref` targets. Workaround: each delete keeps the whole document as an unnamed file in the cache dir and Undo swaps it in (`PdfDoc::snapshot`, ef3fba5e).
- **An appearance stream comes without what places and paints it.** Its `/Matrix`, `/BBox` and resources are not exposed, so another editor's ink taken off and drawn again (an undone erase, a move with Adjust) comes back as straight lines through its `/InkList` in its first path's colour and width, losing its popup, dates and `/IT`. Workaround: none beyond keeping its `/InkList`, note and author (NOTEPAD).
- Smaller gaps:
  - The appearance stream's `/BBox` grows only when `/Rect` changes: a move is a delete and a redraw under one lock (42db32bf).
  - `links()` (`FPDFLink_Enumerate`) reports a link that follows a highlight twice: `PdfDoc::links` reads the annotations itself (9042232f).
  - `FPDF_BOOKMARK` is private, so a cyclic outline is capped rather than caught (`PdfDoc::outline`).
  - `segments()` drops characters at a segment's edges: `text::line_groups`.
  - There is no blend-mode getter (alpha stands in) and no regular-expression search (not offered over a PDF).
  - A second `Pdfium::new` asserts: one instance in a `OnceLock` (`pdf::PDFIUM`).
  - The bindings' `pdfium_latest` is pdfium 7881 while `make pdfium` fetches 8076.
  - An Obsidian `selection=` link counts PDF.js text items, which pdfium segments differently: `text::selection_link` carries the quads and reads the numbers as a hint.

## GLib and GIO

- **A future polled inside another future's poll aborts.** The print dialog and the printer lookup run nested main loops, which do exactly that. Workaround: `export::printed` and the diagram's print start from an idle.
- Smaller gaps:
  - gio has no untrash, so the trash toast has no Undo (NOTEPAD, Deferred).
  - `create_file_for_arg` gives an `ssh://` argument no path: `Shell::command_line` parses the address itself.
  - `spawn_async` without `SEARCH_PATH` looks only in `/bin:/usr/bin`: `terminal::spawn`.
  - glib has no shared future: `lang::flush` polls a `Cell` every 2 ms (NOTEPAD).
  - Removing a spent `SourceId` is a critical: `pdf/tab.rs::save_soon` keeps track.
  - A sandbox with no trash portal answers `NotSupported`: `fileops::trash_all` offers a permanent delete.
  - GApplication would take ssh's askpass prompt for a file to open: `askpass::ask` runs under an id of its own with `NON_UNIQUE`, at the cost of a generic switcher icon.
  - `printerr_literal` needs GLib 2.80, which the build's features do not enable: `shell::resolve` uses `eprintln!`. **Floor: GLib 2.80.**

## notify and inotify

- **Registering the watch set is quadratic.** notify-debouncer-full's `add_root` scans its roots linearly on every `watch()`: unmeasurable at 2 700 directories, seconds at 50 000. Workaround: gitignored trees never reach the watch set, so only a project tracking that many directories pays it (NOTEPAD).
- **inotify's queue overflows on our own walk.** A recursive watch re-adds every skipped tree, and the walk's own directory opens overflow `max_queued_events` (16 384) into a rescan that loops. Workaround: one non-recursive watch per kept directory, polling past 80 % of the watch budget, and a 2 s floor between rescans (`watch::Watcher::new`, `WALK_FLOOR`; 976134bc, 8056ef6e); a vault whose kept directories still outnumber the queue walks once per walk + 2 s (NOTEPAD).
- Smaller gaps:
  - notify-debouncer-full 0.6.0 delivered some events inside the timeout twice: fixed in 0.7.0, which we use (bf689e7c).
  - Dropping a debouncer discards up to 300 ms of pending events: `Watcher::set_dirs` changes the running one (d3e3b523).
  - `IN_OPEN` made our own `git status` read of `.git/HEAD` a change: `watch::classify` drops access events (64c1b841).
  - An atomic replace arrives as Remove + Create once the debouncer has the inode cached: `worker::apply` stats again before believing it.
  - A folder moved in reports nothing about what it holds: that folder is walked (`worker::walk_scope`, 2bc53be8).
  - A folder removed and made again within one batch says nothing of what the old one held: `worker::apply` walks it (9919ce6d).

## SQLite

- **A deferred transaction that writes gets SQLITE_BUSY at once.** SQLite runs no busy handler on a read-to-write promotion, so WAL and a 5 s timeout did nothing against rusqlite's default deferred transaction: "database is locked" between an autosave's reindex and the git refresh. Workaround: every write is `BEGIN IMMEDIATE` (`Index::write_tx`, f2af25d0).
- Smaller gaps:
  - `lower()` is ASCII-only and FTS5's `remove_diacritics` fold is not exposed to SQL, so snippets fold by a table of ours (`search::fold_char`, 9b0a1b18).
  - FTS5's `snippet()` took 2.4 s for a one-character prefix, so snippets are cut in Rust (4cae5cd1).

## Other Rust crates

- **pulldown-cmark 0.13.4 panics on ten bytes.** `"- [ ] \\\n\t-"` makes it slice `6..5` in its heading-attribute block, which took the window down. 0.13.4 is the newest release (crates.io, checked 2026-10-05); an upstream issue is not checked. Workaround: `ENABLE_HEADING_ATTRIBUTES` is off, so `{#id}` after a heading renders as text (`markdown::options`; 086456b2, 95577be2). Goes with a fixed release, though a bump moves every highlighted span and waits for that.
- Smaller gaps:
  - pulldown-cmark's event ranges do not start at their delimiters (`markdown/spans.rs` reads them back, 086456b2) and give reference definitions no position (`links::definitions`).
  - pulldown-latex 0.8 never reports a failure and rejects some valid LaTeX: `markdown::html::mathml` collects the events first, and `Notes::diagnose` warns rather than errs.
  - The `ignore` walker cannot apply gitignore to directories alone, and `WalkState::Quit` stops one pass silently: `walk::dir_ignores`, `walk_passes`.
  - vt100 drops the bottom rows on a shrink and does not expose its parser state, so an escape half-written at a resize garbles the holder's screen until the next redraw (`hold/screen.rs`, NOTEPAD).
  - uniffi 0.32 carries no `usize`, `Range`, `PathBuf`, tuple, `char` or array (`ffi/convert.rs`; a colour crosses as one `u32`), keeps its metadata in the symbols the Android profile strips (`make bindings` reads a host build), and cannot interrupt a call (the ffi's PDF calls are small synchronous units).
  - tempfile creates files 0600: `fs::write_bytes` sets 0666 and leaves it to the umask.
  - For the shell holder, `openpty` sets no close-on-exec, musl sets no errno on the pty call, `TIOCSWINSZ` is typed differently in glibc and musl, and std hands the child its blocked signal mask: `hold/daemon.rs::open_pty`, `set_winsize`, a `sigprocmask` in `pre_exec`.
  - tokio's `abort` does nothing to a blocking thread: a dropped `Task` sends `cancel`.
  - `lsp-types` is unmaintained and `async-lsp` pins an old one: `crates/lsp` transcribes the types (DESIGN.md).
  - serde drops unknown keys silently: `config::unknown_keys` names them.

## git, OpenSSH and other tools

- **git**: `git status` rewrites the index and woke our own watcher (`GIT_OPTIONAL_LOCKS=0`, 347227ea); `submodule status` fails for every submodule once one gitlink has no `.gitmodules` entry (`git::gitlinks` reads the index; f2fae4c3, 0637da81); `GIT_TERMINAL_PROMPT=0` does not bound a silent host (`git/proc.rs::bounded`, `FETCH_TIMEOUT`); a killed git leaves `index.lock` and its hook running (`setsid` and a process-group kill, 280bb7da); a fetch beside a pull fails with "cannot lock ref" (`Panel::fetch_lock`); git reads `SSH_ASKPASS` as the last fallback, so an https username is asked in a password field (NOTEPAD).
- **OpenSSH**: `ssh -O check` asks only the local master process, so a master that opens no session on the host is retired with `-O stop` after a failed command (8d965111); every channel shares one TCP connection, so a running download holds up every call behind it (NOTEPAD); `-t` makes `~` an escape character (`-e none`); ControlPath tokens overflow the 108-byte socket path (`ssh::control_path`); `ControlMaster=yes` over an existing socket leaves no master (`auto` with `ControlPersist`); exit 255 means a lost link and a failed command alike, so the holder fails with 254; the host never hears that a link died, so `serve` is pinged every 10 s and quits after 90 s of silence; `cat` takes a cut-short upload as complete, so the bytes are counted.
- **texlab 5.26** numbers headings by looking their titles up in the `.aux` (wrong for headings titled alike), lists every math environment, offers only `.tex` files inside `\input{` and maps no position between a PDF and its sources: `language/latex.rs` numbers from the `.aux` in order (fb3b58aa, 67cc2654) and lists the other files (7f564233), and SyncTeX is a parser of ours (`accent_core::synctex`). Its dependency graph is not exposed, so two `\input` files headed alike can read the same numbers (NOTEPAD).
- **rust-analyzer** runs `cargo check` only on `didSave`, so the client sends it (1362db7c), and reports every check as progress, so no busy indicator is shown for it.
- **Syncthing** sets a file's mtime or writes the same bytes again, which read as a change on disk: a tab keeps a blake3 digest of what it last read or wrote (`SaveState::digest`, de218477).
- **GNOME Shell** keeps `Ctrl+Alt`+arrows for its workspaces, so Move Divider's chords never arrive unless those are rebound (NOTEPAD).

## Icon themes

- **WhiteSur's `pan-*` icons draw nothing.** They use a single-quoted `fill` that GTK's symbolic recolouring cannot parse, silently. Workaround: icons are named for what the control does (`edit-find-replace-symbolic`, 67cf3870), the Git pane's disclosures are `go-*` (90a23ea4), and `install_chrome_css` points libadwaita's tree expander and dropdown arrow at `go-next-symbolic` / `go-down-symbolic` (320c29d4). `AdwComboRow`'s arrow is a `GtkImage` its template names `pan-down-symbolic`, which no stylesheet can rename: left to the theme (decided 2026-10-03, NOTEPAD).
- Smaller gaps:
  - Themes ship `file-*` names of their own, searched before our GResource, so ours are `filetype-*` (1f18bf7d).
  - Adwaita has no glyph for a function, a type parameter or the drawing tools, so a tabler set is shipped (`apps/gtk/data/icons/LICENSE.tabler`).
  - WhiteSur's `network-transmit-receive` is a device icon, so Sync uses `mail-send-receive-symbolic`.
  - No theme has a dot cursor for the pen's tip: `pdf/view.rs::dot_cursor` draws one (1ff7daf5).

## Flatpak and the GNOME runtime

- **The GNOME 50 runtime has no VTE for GTK 4.** The manifest has no vte module and, as recorded, the runtime ships no `libvte-2.91-gtk4` (not checked against the runtime itself), so the terminal does not build there; a sandboxed shell would also see the runtime rather than the host and want `flatpak-spawn --host`. Deferred with the Flatpak build to the first tag (NOTEPAD).
- Smaller gaps:
  - The GNOME 50 runtime ships no libspelling: the manifest builds it (0.4.10).
  - The rust-stable extension has no musl targets: `make server` builds the remote servers outside the sandbox, which a Flathub build cannot (NOTEPAD).
  - Neither the portal nor a narrower `--filesystem=` can follow a vault's symlinks: `--filesystem=host`.
  - A bare runtime has no OpenType MATH font, so MathML's radicals and delimiters do not stretch; bundle one if the Flatpak shows it (NOTEPAD).
  - GtkSourceView 5.22 comes with the GNOME 51 runtime (see the minimap assertion above).

## Android

- **compose-foundation 1.13 is alpha and forces `compileSdk` 37.** The editor needs `TextFieldBuffer.addStyle`, which exists from 1.13 only, and `foundation-android:1.13.0-alpha03` declares `minCompileSdk=37` (minor 1), so `compileSdk` is 37.2 while `targetSdk` is 36 (5e4dce10). Goes when 1.13 is stable and its released AAR allows 36 (NOTEPAD).
- **The Storage Access Framework cannot see an existing Syncthing folder**, and is 25–50× slower. Workaround: all-files access (`MANAGE_EXTERNAL_STORAGE`) and real paths, which Play allows only behind a declaration (MOBILE_DESIGN.md).
- **inotify over emulated storage drops events.** Workaround: no watcher, and a rescan on every return to the app (`Vault::open_unwatched`, d713e26a).
- Smaller gaps:
  - NDK 25 links 4 KB-aligned, which 16 KB-page devices refuse: `max-page-size=16384` in `.cargo/config.toml` (b46a5102). **Floor: NDK r27.**
  - JNA's AAR ships no R8 rules: `app/proguard-rules.pro` keeps what JNA reaches by name (dd9df6fa).
  - A static musl build refuses cdylibs, so the Android library is a crate of its own (`android/ffi`, 2ed31386).
  - Compose `Constraints` pack at most 32 766 / 65 534 px, which sets the PDF zoom ceiling (`PdfScreen.ceiling`, NOTEPAD); `requiredWidth` re-centres oversized content (`Modifier.oversize`).
  - BitmapFactory ignores EXIF orientation (`uprightImage`, 6b527564), decodes a GIF's first frame only (so a GIF is not recoloured, NOTEPAD), has no TIFF decoder, and may hand back a 16-bit PNG as half floats (not recoloured).
  - The WebView's find-in-page cannot fold accents or see through markup, so some search hits open unmarked (NOTEPAD); a WebView told to load again goes back to the top; its memory cache is app-wide.
  - The WebView consumes the pointer changes it sees and cannot scroll before it has drawn: `Modifier.onTap`, `postVisualStateCallback`.
  - FUSE `/sdcard` has no POSIX modes (the EPERM a `chmod` gets is ignored, `fs::write_bytes`), and there is no trash, so a delete there is permanent.
  - The emulator image refuses root, so no pinch can be driven in a test.

## Xvfb and X11 (the drill environment)

- An `AdwAlertDialog` takes no click and `gtk::FileDialog` does nothing without a portal: drills emit `response` by hand, and dialogs are checked in a real session.
- With no window manager nothing gets the keyboard but through `build-aux/xtest.py`'s `focus`, and a tab dropped on the desktop cannot be tried.
- GTK's a11y registration on a bus with no registry aborts (`GTK_A11Y=none`, c995a2ca), and a drill's WebKitWebProcess can still abort as the drill quits: `G_DEBUG=fatal-criticals` reaches it, and its AT-SPI setup meets the bus `dbus-run-session` has just closed (NOTEPAD).
- `DISPLAY` alone leaves GDK on a running Wayland compositor: `GDK_BACKEND=x11` (`XVFB_ENV` in the Makefile).
- No portal for a theme switch (`ADW_DEBUG_COLOR_SCHEME=prefer-dark`, 19623e1d); a drag crosses no process boundary (a `GdkFileList` is faked, so `connect_drop` is untested); no stylus, touchpad or touchscreen.
- The minimap's assertion (GtkSourceView above) keeps drills to the minimap off.
