# accent design guidelines

These rules cover the GTK4 + libadwaita desktop app, which is what exists today; each section also carries a Material 3 note so the Phase 3 Compose app starts from the same decisions instead of a second design pass. The precedents are Apostrophe and GNOME Text Editor: the note fills the window, the chrome stays out of the way, nothing is decorative. Where this document is silent the GNOME Human Interface Guidelines decide (<https://developer.gnome.org/hig/>, <https://developer.gnome.org/hig/principles.html>). Every URL below was checked and resolves.

## Principles

1. The note is the UI. Everything else is scaffolding that earns its pixels or disappears.
2. One accent colour, taken from the system. No second brand colour, no decorative chrome.
3. Every action is reachable from the keyboard, and the palette is the discoverable index of them.
4. Light and dark are the same design, not two designs. Same for the seven system accents.
5. The system owns fonts, colours, animation and scaling. We only own layout and behaviour.

## Layout map

Phase 1 widget choices. <https://developer.gnome.org/hig/patterns/containers/header-bars.html> · <https://developer.gnome.org/hig/patterns/nav/sidebars.html>

| Surface | Widget |
|---|---|
| Window shell | `AdwApplicationWindow` > `GtkPaned`, one `AdwToolbarView` per side, each with its own `AdwHeaderBar` |
| Header bars | two, so the sidebar reaches the top of the window and the tab bar spans only the editor column. Sidebar header: the start window controls, and otherwise empty. Main header: the sidebar toggle (always visible, so a collapsed sidebar can be brought back without the keyboard), `AdwWindowTitle` with vault name and note path, view-mode toggle group, indexing status label, primary menu, end window controls. The main header takes over the start window controls when the sidebar is hidden |
| Sidebar | `AdwInlineViewSwitcher` in icon mode over an `AdwViewStack` of four equal panes: Files, Search, Tags, Backlinks. The switcher is a top bar of the sidebar's `AdwToolbarView`, not part of its content, so it shares a band with the tab bar. Width is dragged on the `GtkPaned` handle, floor 200 |
| Editor | `sourceview5::View` inside an `AdwClamp` inside a `GtkScrolledWindow` |
| Preview | read-only WebKitGTK 6 view, same clamp width, stylesheet generated from `AdwStyleManager` |
| View modes | Editor / Split / Preview; Split is a `GtkPaned` of the two above |
| Tabs | `AdwTabView` + `AdwTabBar` inside the editor column only, bar hidden while a single tab is open |
| Palette | one `AdwDialog` with a `GtkSearchEntry` and a `GtkListView`; a leading `>` switches file mode to command mode (VS Code convention) |
| Start screen | `AdwStatusPage` with app icon, Open Vault button and a recent-vaults list, shown when launched without a vault path |
| Preferences | `AdwPreferencesDialog` with `AdwPreferencesPage` / `AdwPreferencesGroup` / `AdwSwitchRow` |
| Conflict | side-by-side line diff in an `AdwDialog`, built as a reusable widget so Phase 4 can show git diffs in it |
| Empty states | `AdwStatusPage`: no vault, no note open, no search results, no backlinks |
| Feedback | `AdwToast` / `AdwBanner` / `AdwAlertDialog`, see States |
| Loading | the header bar status label (`.dim-label`); never a modal, never a blocked window |

## Typography

<https://developer.gnome.org/hig/guidelines/typography.html> · <https://developer.gnome.org/hig/guidelines/writing-style.html>

- Prose uses the GNOME **document** font from `AdwStyleManager::document_font_name()`, applied to `textview.accent-doc` in `main.rs::install_document_font` and re-applied when the setting changes. Notes are prose, not code. libadwaita 1.7 also exposes `--document-font-family` / `--document-font-size` as CSS variables, which is the upgrade path away from the hand-built provider.
- Monospace appears only inside the `code`, `codeblock`, `math`, `html` and `frontmatter` text tags.
- Heading scale in `highlight.rs`, relative to the document font: h1 1.6, h2 1.4, h3 1.2, h4 1.1; h5 and h6 are bold at 1.0. `strong` and list markers are weight 700, `em` is italic.
- Line length is set by width, not by counting characters. The editor wraps its view in an `AdwClamp`, `maximum-size` 800 and `tightening-threshold` 600, and the preview caps its column at `56ch` (measured at 71 characters in Adwaita Sans, 62 in Cantarell). Measure rather than assume: `ch` is the width of a zero and much wider than an average letter.
- **The editor is deliberately wider than the readable ideal.** 60 to 72 characters is where typography research puts comfortable prose, and the clamp was 580 to match it. The user asked for a document column of at least half the viewport, which puts the editor at roughly 96 characters. The width won because it is what the person writing in it wanted; the dial is `maximum-size` in `editor.rs` if that ever reads as too long.
- Header capitalisation for buttons, menu items, tab titles and tooltips; sentence capitalisation for messages and descriptions. Ellipsis (…) only when the label needs further input before acting. No OK / Yes / No buttons: label the affirmative button with its verb, for example Save or Discard.

## Colour

<https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.7/css-variables.html> · <https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.7/style-classes.html>. libadwaita's old `named-colors.html` page is gone (it 404s) and CSS variables replaced it, so `@named_color` syntax is out.

- The only three colour sources in code are `StyleManager::accent_color_rgba()`, `Widget::color()` (the resolved theme foreground) and `StyleManager::is_dark()`.
- GTK CSS uses `var(--accent-bg-color)`, `var(--view-bg-color)`, `var(--window-fg-color)` and friends. Never `@named_colors`, never a literal hex.
- Editor tag colours, all derived, in `highlight.rs::restyle`: `link`, `wikilink`, `tag` and `image` take the accent at full alpha; `marker`, `frontmatter` and `listmarker` take the foreground at alpha 0.4; `quote` and `taskdone` the foreground at alpha 0.6; `code` and `codeblock` get a foreground background at alpha 0.07. 23 tags in total; no other colour is set anywhere.
- The preview stylesheet derives everything from three values: foreground, background, accent. WebKitGTK cannot see GTK's CSS variables, so the light/dark background pair is written out as `#ffffff` / `#1d1d20` (libadwaita's `--view-bg-color`). That pair is the only hex allowed in the codebase; anything else is a bug the pre-flight grep catches.
- One flat background. The sidebar, both header bars, the tab bar and the document all paint `var(--view-bg-color)` through the `accent-flat` class, so the window reads as one surface rather than banded panels. The 1 px paned separator is the only division.
- Light/dark parity is by construction: nothing is picked per theme, so there is no second palette to keep in sync. Same for the accent, which the user can change at any moment.

## Spacing

The scale is 6, 12, 18, 24, 36 and nothing between: 6 inside a control group, 12 between related widgets, 18 between groups, 24 for dialog and page padding, 36 for empty-state breathing room. (The current HIG has no spacing page; this is the long-standing GNOME convention.) Current values: window 1100x760; sidebar floor 200, dragged on the paned handle and remembered in the session; editor margins left 48, right 48, top 24, bottom 96, with 2 px above and below lines (`main.rs`, `editor.rs`). Two vertical `GtkSizeGroup`s keep the two columns in one horizontal rhythm whatever the interface font: one across the two header bars, one across the switcher row and the tab bar. The second is held at `SizeGroupMode::None` while a single tab hides the tab bar, so no empty band is reserved above the document. The editor margins sit off the scale on purpose: they are page gutters inside the clamp, not layout spacing.

## Chrome auto-hide

The point of the app. Header bar and tab bar fade out while the user types and come back the moment attention leaves the text, in every view mode.

- Hides: the header bar and the tab bar. Trigger: the first keystroke into the editor buffer.
- Returns on pointer motion anywhere in the window, Escape, focus change, a view-mode change, an action fired from the palette or a menu, and any keyboard focus move out of the editor. Hover must never be the only route back, or a keyboard-only user is stuck with hidden chrome.
- Never hides: the sidebar, the editor, toasts, banners, dialogs. Suspended entirely while a dialog, banner, popover, the palette or the find bar is open.
- Implementation: a CSS `opacity` transition on a `.chrome-hidden` class. Opacity only, so the layout never shifts and the widgets keep their size and focus order. `AdwToolbarView:reveal-top-bars` is the named upgrade path if the CSS approach ever fights the toolbar view (<https://gnome.pages.gitlab.gnome.org/libadwaita/doc/1.7/class.ToolbarView.html>).
- With `gtk-enable-animations` false the class still toggles but the transition is zero-length, so chrome jumps instead of fading and nothing becomes unreachable (<https://docs.gtk.org/gtk4/property.Settings.gtk-enable-animations.html>).

## Keyboard

Every user-facing action is a `GAction` with an accelerator and an entry in the palette's command mode. Without an action it cannot be scripted, tested or found; without a palette entry it does not exist. <https://developer.gnome.org/hig/guidelines/keyboard.html> · <https://developer.gnome.org/hig/reference/keyboard.html>

| Group | Bindings |
|---|---|
| Files | Save `Ctrl+S`, New note `Ctrl+N`, New folder `Ctrl+Shift+N`, Close tab `Ctrl+W`, Quit `Ctrl+Q` |
| Palette and find | Files `Ctrl+P`, Commands `Ctrl+Shift+P`, Find `Ctrl+F`, Replace `Ctrl+H`, Find next / previous `Ctrl+G` / `Ctrl+Shift+G` |
| Panes | Sidebar `F9`, Files / Search / Tags `Ctrl+Shift+E` / `Ctrl+Shift+F` / `Ctrl+Shift+T`, Backlinks `Ctrl+Shift+B`, Cycle view mode `Ctrl+E` |
| Notes | Follow link `Ctrl+Return`, Rename `F2`, Move to trash `Delete` (tree only), Daily note `Ctrl+Shift+D` |
| Window | Fullscreen `F11`, Preferences `Ctrl+comma`, Primary menu `F10` |
| Tabs | `Ctrl+Tab`, `Ctrl+PageUp` / `Ctrl+PageDown`, `Alt+1` to `Alt+9` |

Never bind: `Super`+anything (the shell owns it), `Alt+Tab`, `Alt+F4`, `Alt+F7`, `Alt+F8`, `Ctrl+Alt+*` (workspace switching), `F1` (help), `Ctrl+Shift+U` (IBus unicode entry), `Ctrl+Space` (input-method switch), `Ctrl+D`, and the GtkSourceView built-ins (`Ctrl+Z`/`Ctrl+Y`, `Ctrl+A`, `Ctrl+X`/`C`/`V`, `Ctrl+K`, `Alt+Up`/`Alt+Down`, `Ctrl+Home`/`Ctrl+End`).

One deliberate HIG deviation: the HIG reserves `Ctrl+P` for Print and `Ctrl+Shift+P` for Print Preview. accent has no printing, and its users arrive from VS Code and Obsidian where both open the palette. Revisit if printing is ever added.

There is no shortcuts window. `AdwShortcutsDialog` needs libadwaita 1.8 and we build against 1.7; `GtkShortcutsWindow` is deprecated since GTK 4.18 and will be removed in GTK 5. Until the libadwaita floor moves to 1.8 the palette's command mode is the shortcuts reference, so it must show the accelerator next to every command.

## Iconography

Symbolic icons from the Adwaita theme only: no bundled glyphs, no emoji (<https://developer.gnome.org/hig/guidelines/ui-icons.html>). All of the following were confirmed present in `/usr/share/icons/Adwaita/symbolic/` on the development machine (adwaita-icon-theme 50):

`sidebar-show-symbolic`, `open-menu-symbolic`, `document-new-symbolic`, `folder-new-symbolic`, `document-save-symbolic`, `document-edit-symbolic`, `document-open-recent-symbolic`, `document-revert-symbolic`, `edit-find-symbolic`, `edit-find-replace-symbolic`, `system-search-symbolic`, `edit-clear-symbolic`, `view-dual-symbolic`, `view-reveal-symbolic`, `view-fullscreen-symbolic`, `view-refresh-symbolic`, `user-bookmarks-symbolic`, `insert-link-symbolic`, `x-office-calendar-symbolic`, `user-trash-symbolic`, `folder-symbolic`, `text-x-generic-symbolic`, `x-office-document-symbolic`, `window-close-symbolic`, `preferences-system-symbolic`, `dialog-warning-symbolic`, `object-select-symbolic`, `go-previous-symbolic`, `go-next-symbolic`, `list-add-symbolic`.

This theme has no `tag-symbolic`, so the Tags pane uses `user-bookmarks-symbolic`. If a glyph is missing: take the closest existing Adwaita name first, and only if nothing fits ship one in the app `GResource` under the `io.github.stroblme.Accent` prefix, drawn on the 16 px symbolic grid with `fill="currentColor"` so it recolours with the theme. Never ship a coloured icon.

## States

- Empty: `AdwStatusPage` with a symbolic icon, a header-capitalised title, one sentence of body text and at most one button (<https://developer.gnome.org/hig/patterns/feedback/placeholders.html>).
- Loading: the header status label, as `start_reconcile` already does ("Indexing… 1200/42700 files"). Indexing and saving never block the window, so no spinner covers content and no progress bar owns the window.
- Toast for a thing that happened and is over ("Saved", "Moved to Trash" with an Undo button). Banner for a state that persists and needs a decision ("This note changed on disk", "Conflict copy found"). `AdwAlertDialog` only when the choice can lose data: overwrite, discard, delete permanently. <https://developer.gnome.org/hig/patterns/feedback/toasts.html> · <https://developer.gnome.org/hig/patterns/feedback/banners.html> · <https://developer.gnome.org/hig/patterns/feedback/dialogs.html>

## Motion

libadwaita defaults only: no custom easing, no staggered reveals, nothing that animates on load. The only timings we own are debounces, and they exist to keep the main loop free.

| Timer | Value | Where |
|---|---|---|
| Re-highlight after a keystroke | 150 ms | `editor.rs::DEBOUNCE` |
| Palette / switcher query | 50 ms | `switcher.rs::DEBOUNCE` |
| Preview re-render (Phase 1) | 300 ms | preview pane |
| Autosave (Phase 1) | 1 s idle | editor tab |

## Material 3 mapping (Phase 3)

<https://m3.material.io/> · <https://developer.android.com/develop/ui/compose/designsystems/material3>

| libadwaita | Compose Material 3 |
|---|---|
| `AdwApplicationWindow` + `AdwToolbarView` | `Scaffold` with `topBar` and `contentWindowInsets` |
| `AdwHeaderBar` | `CenterAlignedTopAppBar` |
| `AdwOverlaySplitView` | `ModalNavigationDrawer` on phone, `PermanentNavigationDrawer` on tablet |
| `AdwInlineViewSwitcher` + `AdwViewStack` | `PrimaryTabRow` + `HorizontalPager` |
| `sourceview5::View` in `AdwClamp` | `BasicTextField` with an `AnnotatedString` built from the same core spans, width-capped by `Modifier.widthIn` |
| WebKitGTK preview | Compose markdown renderer over the same `markdown::to_html` output |
| `AdwTabView` + `AdwTabBar` | `ScrollableTabRow`, or a bottom sheet listing open notes on phone |
| Palette `AdwDialog` | full-screen `SearchBar`, same `>` command prefix |
| `AdwStatusPage` | centred `Column` with icon, `headlineSmall`, `bodyMedium`, one `FilledTonalButton` |
| `AdwToast` / `AdwBanner` / `AdwAlertDialog` | `Snackbar` / inline `Card` / `AlertDialog` |
| `AdwPreferencesDialog` | settings screen of `ListItem` rows with `Switch` trailing content |
| `StyleManager::accent_color_rgba()` | `dynamicLightColorScheme` / `dynamicDarkColorScheme` from the system accent (API 31+), fixed seed below that (<https://m3.material.io/styles/color/dynamic-color/overview>) |
| Chrome auto-hide | `AnimatedVisibility` on the top bar driven by a `nestedScroll` connection and `WindowInsets.ime` visibility instead of pointer motion (<https://developer.android.com/develop/ui/compose/animation/composables-modifiers>) |

## Pre-flight checklist

Ten mechanical checks before shipping a UI change. None of them needs judgement.

1. `cargo fmt --all --check`
2. `cargo clippy -p accent --all-targets --locked -- -D warnings`
3. `cargo test --locked && cargo test -p accent --locked`
4. Headless smoke run (ROADMAP §6): `Xvfb :99 & DISPLAY=:99 G_DEBUG=fatal-criticals target/release/accent testvault`
5. Dark: `gsettings set org.gnome.desktop.interface color-scheme prefer-dark`, look, then set it back to `default`
6. Accent: `gsettings set org.gnome.desktop.interface accent-color teal`, look, then set it back to `blue`
7. No stray colours: `grep -rnE '#[0-9a-fA-F]{3,8}' apps/gtk/src` returns only the preview background pair
8. Reduced motion: `gsettings set org.gnome.desktop.interface enable-animations false`, check nothing became unreachable, then set it back to `true`
9. Keyboard-only pass with the pointer unplugged: reach every action in the accelerator table, and get the auto-hidden chrome back without a mouse
10. `GDK_SCALE=2 target/release/accent testvault` for scaling, plus an IME check (ibus, type CJK into a note and confirm the preedit lands in the right place)
