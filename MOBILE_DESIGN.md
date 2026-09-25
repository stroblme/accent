# accent on Android — design rules

What DESIGN.md says still holds: the note is the UI, one accent taken from the system, light and
dark are one design, and the system owns fonts, colours, motion and scaling. This file is the part
that cannot be carried across, because a phone is not a small desktop. Where these rules and
Material 3 disagree, these win: the reference is the content-first, flat, typographic style of apps
like Trade Republic, not Material's cards and elevation. DESIGN.md's Material 3 table says which
Compose widget stands in for which desktop one.

## Principles, restated for a phone

1. **One thing at a time.** No split, no second pane, no tab bar: the screen holds the note or the
   way to another note, never both. What the desktop puts side by side, a phone puts behind a
   gesture.
2. **The page is flat.** No cards, no elevation, no tonal containers: every `surfaceContainer*` tone
   is collapsed onto the surface (`ui/Theme.kt`). A list is rows on the page; a sheet is the page
   sliding up.
3. **Hierarchy is typographic.** Size and weight say what matters. Colour says one thing only — this
   is interactive — and it is the system's accent.
4. **A finger never draws.** Touch moves the page; the stylus draws, as on the desktop.
5. **Reading is the default.** A note opens rendered; editing is a deliberate act, because a
   keyboard takes half the screen.

## Layout

### Vault picker

- A centred column: the name, one line of explanation, the permission if it is missing, one button,
  then the recent vaults as plain rows.
- A pick leaves it on the next frame, for the vault's own screen reading "Reading your vault…" over
  the indeterminate bar while the core is still opening the index; a folder that cannot be opened
  comes back here and says why in a `Snackbar`. A picker that stays put after a tap reads as a tap
  that was lost.

### Browse

- A panel over what is being read, with the Search / Files / Command chips and the query field at
  the foot of the screen, nearest the thumb. The desktop's eight sidebar panes and its palette
  collapse into it.
- Search is the default: the tree until there is a query, then what the notes say. Files is the
  switcher — the recent files, then names ranked against the query — and Command the palette; both
  lay their rows out from the bottom up, so the best match is nearest the thumb, and both put the
  keyboard up when their page lands.
- A handle at the top, and a pull down anywhere in it closes it. No Close button: Back already does
  that, and a second way out costing a corner of the screen is a corner spent twice.

### Note

- Reading: the rendered note in a WebView, 16 dp side gutters, one thin bar over the top of it and,
  while a find is open, the find bar below. The page is loaded when the note or the palette changes
  and at no other time: a WebView told to load again sends the reader back to the top.
- Editing: the same text with the same styling spans, markup visible and dimmed.

### PDF

- A column of pages with the note's bar drawn over them, not above them: a page is read at a zoom
  and an offset the reader chose, and chrome that takes layout space moves both each time it comes
  and goes.
- Back first retraces the jumps taken inside the document — a link followed, a bookmark picked —
  latest first, as the desktop's history does; with none left it leaves the document.
- The highlights the vault's notes make by linking into a passage
  (`[[paper.pdf#page=…&selection=…]]`) are painted over the page in the accent at 20 %, always, as on
  the desktop. A tap on one opens the note that makes it, marked at the text the link quotes, and
  Back from that note returns to the page, the zoom and the pan it was left at.
- Following such a link from a note opens the PDF on its page with the passage selected and brought
  to the middle of the screen; numbers that fit no line of the page open the page.
- A long press selects the word under the finger and the drag after it grows the selection; two
  accent handles then move either end, across a page break as readily as within a page — the
  desktop's drag in the platform's shape. The platform's floating toolbar offers Copy and Copy link.
- Copy link puts the desktop's own link on the clipboard,
  `[[paper.pdf#page=N&selection=a,b,c,d|the quoted text]]`, one per page the selection covers, and
  pasting it into a note is what makes a highlight. A PDF opened from another app is named by the
  file name its provider gives, as the desktop names a file from outside a vault.
- A tap while text is selected lets the selection go and does nothing else.
- Find — the bar's or the palette's — searches the whole document a page at a time, from the page
  under the middle of the screen round to the one before it, lands on the first match at or after
  that page and steps round the ends; the matches are at the desktop's 30 %, the current one at 60 %.
  Its bar sits at the foot of the pages, as a note's does.
- The annotation toolbar is off (`PdfScreen.ANNOTATIONS`) until its design settles: until then no
  tool can be picked and a finger only ever moves the page.

### Message

- A `Snackbar`. A state that needs a decision is an inline row above the content, not a dialog —
  except on the way out, where the row would go with the content: leaving a note over edits whose
  saving was paused asks the row's question in an `AlertDialog`.

## Spacing and type

- Side gutter 16 dp everywhere, on every screen, at every width. Rows 56 dp.
- The scale is 4 / 8 / 16 / 24 / 32, and nothing between.
- Body text is the system's own size, the one the reader chose; everything else is measured against
  it: `headlineSmall` for a screen's title, `titleMedium` for a note's, `labelMedium` in the muted
  colour for anything secondary.
- Dividers are hairlines or absent: a list needs neither a box nor a rule to read as a list.
- One radius scale on the theme (`ui/Theme.kt`), 8 / 12 / 16 / 20 / 28 dp, and nothing rounds
  itself: Material's own starts at 4 dp, which on a flat page reads as a rectangle somebody failed to
  round. A query field is the 20 dp step, a floating button the 28 dp one, past half its height,
  which is what makes a pill a pill.

## Gestures

Nothing is keyboard-reachable here, so a gesture is the only affordance, and every one must also
exist as a visible control.

| Gesture | What it does | Its visible twin |
|---|---|---|
| Pull the panel down | Closes Browse | The handle at its top |
| Swipe across the panel | Steps between Search, Files and Command | The chips, which travel with it |
| Tap the content | Puts the chrome up or takes it down | — |
| Scroll on | Takes the chrome down; scrolling back brings it up | — |
| Pinch on a page | Zooms a PDF, 1× to 8× (less on a viewport wider than 4095 px), around the point between the fingers | — |
| Drag on a zoomed page | Pans it, both axes at once | — |
| Tap a highlight on a page | Opens the note whose link makes it; Back returns to the page | The highlight |
| Long press on a PDF page | Selects the word under the finger; a drag grows it, and the handles move either end | The handles and the floating toolbar it raises |
| Long press on a note | Selects the text under the finger with the WebView's own handles, as a page of prose does everywhere else on the platform | The handles it raises |

- A panel is closed by pulling it down, the one gesture here that reads as itself: the handle says
  it moves, and down is where a panel goes. What is inside scrolls first, so only a drag the list
  cannot use pulls the panel, and letting go short of the threshold springs it back. Back does the
  same, so nothing has to be discovered.
- A drag that began by scrolling the list stops where the list does: reaching the top of the files
  is on the way to the first of them and must not also take them away, so closing is a second pull,
  from a standstill — the rule every sheet on the platform follows.
- Browse is a button, not a gesture: an edge swipe or a pull from the top has no visible twin, and a
  gesture nothing announces is one nobody finds. One button, because two pills at the foot of the
  screen are a choice made before the reader knows which one they want; the chips ask it inside,
  where the answer is on screen. It floats at the foot of whatever is being read, goes with the
  rest of the chrome, and goes outright while the keyboard is up.
- A document is one surface, not a vertical scroller wrapped in a horizontal one: two scroll
  containers each claim a drag the moment it looks like theirs, which makes a diagonal drag pick a
  side, while one gesture handler feeding both axes follows the hand and lets a pinch grow the page
  from between the fingers.
- A pinch never lays the column out again: it scales one layer under the fingers and commits once,
  on release. That layer's column is placed by its top-left corner, not by a `required*` modifier,
  which centres what overflows and would shift the page sideways on every pinch; its position is
  never read back in the frame that sets it, which walks the page out from under the hand; and it
  grows in both directions, a column held to the screen's width squeezing the page sideways.

## The document

- A note and a PDF are one surface with two kinds of content: the same `DocumentBar` — the file's
  name and the one thing that can be done to it — over the same `DocumentGap`, the content in the
  rectangle that leaves (`ui/Common.kt`). Moving between a note and a PDF should not move what is
  being read.
- On both, the bar lies over the content, not above it (`DocumentFrame`): a document is read at a
  scroll offset, a PDF at a zoom as well, and a bar sometimes in the layout gives the document two
  positions, so tapping for the bar would move what the tap was aimed at.
- The cost is the head of the document, which the bar covers while it is up and which cannot be
  scrolled clear — mostly page margin on a PDF, the first line of a note, every document opening with
  its bar up. The same tap takes the bar away; a reserved strip would be a permanent gap on a screen
  whose chrome is down most of the time.
- The editor is the exception: its bar never fades, so it keeps clear of it by the bar's measured
  height, outside its scroll — a bar across the line being typed would cost with nothing bought. A
  banner holding a decision goes above the bar, where the bar cannot cover it, and moves the note
  once coming and going.
- The bar's button is a note's Edit and Done. A PDF's are Find and Contents, the document's
  bookmarks; on a file with none, Contents is there and disabled: a gap where a control belongs is
  worse than a control that says it has nothing to offer.
- Only one screen keeps window insets. A PDF inside a vault is inside a screen already clear of the
  status bar, so its own scaffold takes none; opened from another app it keeps them itself. Applied
  twice, the bar sits lower than a note's.

## Chrome

- The desktop fades its chrome while the reader types; a phone has almost none, so the rule becomes:
  the bar over a note and the Browse button go when the content scrolls on, and come back when it
  scrolls back or is tapped; the keyboard hides the button outright. One `Chrome` holds that state
  for the whole screen (`ui/Common.kt`), and whatever scrolls tells it so.
- Two things never fade: the bar while the editor is open, Done being the only way out of it; and
  the content, a message, or anything holding a decision.
- A note's find bar is the exception to the keyboard rule: it stays while the keyboard is up, the
  keyboard being what it is for, and the Browse button goes instead, so the vault's search and the
  page's find are never on screen together. It takes its space from the note rather than lying over
  it — a bar across the last lines would cover the match it just found — and Back puts it away, as
  Back closes the panel.
- Motion is two durations and three curves (`ui/Common.kt`): 200 ms for a surface arriving, 150 ms
  for one leaving and for one stepping sideways — Material's short-4 and short-3, the fast end of
  its scale, since in a reading app a transition that has to be waited for is worse than none.
  What arrives decelerates into place and what leaves accelerates away: arriving is watched,
  leaving is not.
- Nothing asks whether the reader wants less motion: Compose scales every animation by the
  platform's animator duration scale, so a device with animations off already gets none, and a
  check of our own would be a second answer to the same question.

## The launcher icon

- The desktop logo (`data/icons/logo.svg`) as an adaptive icon, its white paper the background
  layer. Its shapes are strokes, so the vector drawable carries `strokeColor` and `strokeWidth`
  rather than fills, and the group is scaled to half size so the whole art fits inside the circle a
  mask may cut.

## Colour

- The device is asked for the accent and for nothing else: Material You answers what GNOME's accent
  does — what colour is this device — so `primary` comes from it, for what the desktop uses the
  accent for: links, the caret, a selected tool, an ink stroke's default.
- The page and its ink are the desktop's: `#333338` on `#ffffff` in light, `#ebebeb` on `#1d1d20`
  in dark, libadwaita's view colours from `apps/gtk/src/theme.rs` (the light ink composited down
  from its 80 % alpha there). A wallpaper-tinted page reads cream beside that, and one vault should
  look like one editor on both. Secondary text and borders are that ink thinned over that page, so
  nothing under the accent is tinted. Those four values are every colour the app ships; the
  launcher icon's paper is the light page.
- The Browse button is the exception: `inverseSurface`, the other mode's pair, dark on a light theme
  and light on a dark one. It is the one thing on screen that is not the document, and a pale pill
  on a pale page is a pill nobody sees; contrast rather than colour, so the accent still means only
  one thing.
- A PDF is recoloured in a dark theme as on the desktop: the document's paper lands on the app's
  surface and its ink on the app's text, each pixel keeping its chroma, so a coloured figure stays
  coloured.

## Architecture

The decisions under the Android app, each with its reason; the shared ones are in DESIGN.md.

- **Scope**: reading the vault, small edits, reading and marking up PDFs, and serving as the
  system's PDF viewer (`ACTION_VIEW`, no vault needed); the Pixel 8 is the reference device.
- **Storage**: all-files access (`MANAGE_EXTERNAL_STORAGE`) and real paths, distributed as an APK
  or through F-Droid, Play optional — the Storage Access Framework is 25–50× slower and cannot see a
  Syncthing folder that already exists, and Play allows the permission only behind a declaration.
- **The ffi** lives in `crates/api/src/ffi/` behind the `android` feature, core types declared with
  `#[uniffi::remote]` — one façade, and no mirrored definitions to keep in step.
- **`android/ffi` is a cdylib of its own** — `accent-api` as one would make every desktop build link a
  shared object nobody loads, and the static musl server build refuses cdylibs.
- **Text offsets become UTF-16 in the ffi** — the core counts bytes and Kotlin UTF-16, and a span in
  the wrong unit lands beside the word.
- **A colour crosses as one `u32`** (`0xRRGGBBAA`) — uniffi carries no arrays, and it is how Android
  spells a colour.
- **No watcher**: `Vault::open_unwatched`, and a rescan on every return to the app — inotify over
  emulated storage drops events; the app's own writes still reach the index.
- **A first index lets the reader in while it runs**: the walk commits in batches and the tree lists
  what it has as it goes — a large vault's first walk is tens of seconds. Stopping it is the
  desktop's pause, and the rescan on a return is one of the walks a pause refuses.
- **The PDF rules are the core's**, called by both apps (`pdf::highlight_quads`,
  `pdf::link_with_alias`, the ink ledger in `accent_core::pdf::ledger`) — the reader behaves as the
  desktop's does, and only the gestures, the drawing and their arithmetic are Kotlin.
- **Settings and sessions are Kotlin's** (`SharedPreferences`) — panes, splits and zoom do not exist
  here; what the two apps share is the vault, and that is a path.
- **The rendered view is a `WebView`** over the core's `markdown::to_html`, with the desktop's
  `accent://` scheme and generated CSS — it is the only thing on the platform that draws MathML.
- **The editor is `BasicTextField(TextFieldState)`** with a styled `OutputTransformation` — the only
  field that is not deprecated, and `TextFieldBuffer.addStyle` is where the core's spans go.
- **Wet strokes are a Compose `Canvas`** path committed on release — about thirty lines and no View
  interop; `androidx.ink` is the upgrade once a stylus has been held against it.
- **minSdk 31** — where Material You reads the system colours the accent comes from.

## What is not here, and why

- Git, language servers, ghost text and word suggestions, remote vaults, the terminal, diagrams,
  multi-pane, comparison, Replace All, the minimap, focus-mode levels and presentation mode are
  desktop features: some have no input model here (a shell without a keyboard), some duplicate what
  the platform does (a phone keyboard completes words), and the rest would be a second app inside
  this one. The test for any feature: does it help someone read their vault, make a small edit, or
  read and mark up a PDF.
- Deferred rather than refused: tags and backlinks, templates and the daily note, mermaid diagrams
  in the rendered view, a native Compose renderer in place of the WebView, exporting highlights, and
  the PDF shapes and Adjust tool.
