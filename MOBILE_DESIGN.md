# accent on Android — design rules

What DESIGN.md says still holds: the note is the UI, one accent taken from the system, light and
dark are one design, and the system owns fonts, colours, motion and scaling. This file is the
part that cannot be carried across, because a phone is not a small desktop. Where these rules and
Material 3 disagree, these win — the reference is the content-first, flat, typographic style of
apps like Trade Republic rather than Material's cards and elevation.

DESIGN.md's Material 3 table maps each desktop widget to its Compose equivalent. It says what to
build with. This says what to build.

## Principles, restated for a phone

1. **One thing at a time.** There is no split, no second pane and no tab bar. The screen holds the
   note, or the way to another note, never both. What the desktop puts side by side, a phone puts
   behind a gesture.
2. **The page is flat.** No cards, no elevation, no tonal containers. Every Material
   `surfaceContainer*` tone is collapsed onto the surface (`ui/Theme.kt`). A list is rows on the
   page; a sheet is the page sliding up.
3. **Hierarchy is typographic.** Size and weight say what matters. Colour says one thing only —
   this is interactive — and it is the system's accent.
4. **A finger never draws.** Touch moves the page; the stylus draws. Same rule as the desktop,
   same reason.
5. **Reading is the default.** A note opens rendered. Editing is a deliberate act, because a
   keyboard takes half the screen.

## Layout

| Surface | What it is |
|---|---|
| Vault picker | A centred column: the name, one line of explanation, the permission if it is missing, one button, then the recent vaults as plain rows |
| Browse | A screen of its own, over what is being read. The tree, or the results when there is a query, with the search field at the *foot* of the screen. The desktop's eight sidebar panes collapse into this one |
| Note, reading | The rendered note in a WebView, full bleed, 16 dp side gutters, one thin bar above it. The page is loaded when the note or the palette changes and at no other time: a WebView told to load again is a reader sent back to the top |
| Note, editing | The same text with the same styling spans, markup visible and dimmed |
| PDF | A column of pages. Leaving the document is Back. The annotation toolbar is off (`PdfScreen.ANNOTATIONS`) until its design settles; while it is off no tool can be picked and a finger only ever moves the page |
| Launch | The switcher, the same shape: rows from the bottom up so the best match is nearest the thumb, then the Notes / Commands chips, then the query field at the foot. The keyboard is up when it opens |
| Message | `Snackbar`. A state that needs a decision is an inline row above the content, not a dialog |

## Spacing and type

- Side gutter 16 dp everywhere, on every screen, at every width. Rows 56 dp.
- The scale is 4 / 8 / 16 / 24 / 32. Nothing between.
- Body text is the system's own size: that is what the reader chose. Everything else is measured
  against it — `headlineSmall` for a screen's title, `titleMedium` for a note's, `labelMedium` in
  the muted colour for anything secondary.
- Dividers are hairlines or absent. A list needs neither a box nor a rule to read as a list.

## Gestures

Nothing is keyboard-reachable here, so a gesture is the only affordance — which means every one
of them must also exist as a visible control.

| Gesture | What it does | Its visible twin |
|---|---|---|
| Tap the content | Puts the chrome up or takes it down | — |
| Scroll on | Takes the chrome down; scrolling back brings it up | — |
| Pinch on a page | Zooms a PDF, 1× to 6×, around the point between the fingers | — |
| Drag on a zoomed page | Pans it, both axes at once | — |
| Long press | The context sheet for the thing under it | — |

Browse and Launch are buttons, not gestures. They were an edge swipe and a pull from the top, and
neither had the visible twin this table asks for — a gesture nothing announces is a gesture nobody
finds. They float at the foot of whatever is being read, go with the rest of the chrome, and go
outright while the keyboard is up.

A document is one surface, not a vertical scroller with a horizontal one wrapped around it. Two
scroll containers each own an axis and each claims a drag the moment it looks like theirs, which
is what makes a diagonal drag pick a side; one gesture handler feeding both axes is what makes it
follow the hand. The same handler is what lets a pinch grow the page away from the fingers rather
than from its top-left corner.

A pinch never lays the column out again. It scales one layer under the fingers and commits once,
on release. Asking the list where it is and telling it where to go in the same frame reads back
the position from before the answer, which walks the page out from under the hand a little more
every frame; and a column held to the width of the screen grows taller without growing wider,
which is a page squeezed sideways. Refreshing happens on its own: the vault is walked again every
time the app comes to the foreground.

## Chrome

The desktop fades its chrome while the reader types. A phone has almost none to fade, so the rule
becomes: the bar above a note and the two buttons go when the content scrolls on, and come back
when it scrolls back or is tapped; the keyboard hides the buttons outright. One `Chrome` holds that
state for the whole screen (`ui/Common.kt`), and what scrolls tells it so.

Two things never fade. The bar while the editor is open, because Done is the only way out of it;
and the content, a message, or anything holding a decision.

## The launcher icon

The desktop logo (`data/icons/logo.svg`) as an adaptive icon: the shapes are *strokes*, not
fills, so the vector drawable carries `strokeColor` and `strokeWidth` rather than painting the
paths in. Its white paper is the background layer. The art's furthest point from centre is a
parallelogram corner plus half its stroke, about 162 of the 256-unit viewport, and a mask can be
a circle of radius 78, so the group scales to half size to sit inside one whole.

## Colour

Material You is the same answer GNOME's accent is: ask the device what colour it is. `primary` is
the one accent and it is used for exactly what the desktop uses it for — links, the caret, a
selected tool, an ink stroke's default. Everything else is `surface`, `onSurface` and
`onSurfaceVariant`. The app ships one colour of its own, the launcher icon's background.

A PDF is recoloured in a dark theme the way the desktop does it: the document's paper lands on the
app's surface and its ink on the app's text, each pixel keeping its own chroma, so a coloured
figure stays coloured.

## What is not here, and why

Git, language servers, ghost text and word suggestions, remote vaults, the terminal, diagrams,
multi-pane, comparison, Replace All, the minimap, focus-mode levels and presentation mode are
desktop features. Some have no input model here (a shell without a keyboard), some duplicate what
the platform already does (a phone keyboard completes words), and the rest would be a second app
inside this one. The test is the one the roadmap set: does it help someone read their vault, make
a small edit, or read and mark up a PDF.

Deferred rather than refused: tags and backlinks, templates and the daily note, mermaid diagrams
in the rendered view, a native Compose renderer in place of the WebView, exporting highlights, and
the PDF shapes and Adjust tool.
