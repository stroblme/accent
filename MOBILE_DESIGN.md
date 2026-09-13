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
| Files drawer | `ModalNavigationDrawer` from the left edge. A search field on top; below it the results when there is a query and the tree when there is not. The desktop's eight sidebar panes collapse into this one |
| Note, reading | The rendered note in a WebView, full bleed, 16 dp side gutters, one thin bar above it |
| Note, editing | The same text with the same styling spans, markup visible and dimmed |
| PDF | A column of pages, one floating toolbar at the bottom end holding the tools and nothing else. Leaving the document is Back; the drawer is an edge swipe; strokes are written back a second after the last one, so there is no Save |
| Switcher | `ModalBottomSheet` with a Notes / Commands chip pair, a query field, and rows |
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
| Drag down from the top of the content | Opens the switcher | The ⋯ on the toolbar |
| Swipe from the left edge | Opens the files drawer | The Files button |
| Pinch on a page | Zooms a PDF, 1× to 6×, around the point between the fingers | — |
| Drag on a zoomed page | Pans it, both axes at once | — |
| Long press | The context sheet for the thing under it | — |

A document is one surface, not a vertical scroller with a horizontal one wrapped around it. Two
scroll containers each own an axis and each claims a drag the moment it looks like theirs, which
is what makes a diagonal drag pick a side; one gesture handler feeding both axes is what makes it
follow the hand. The same handler is what lets a pinch grow the page away from the fingers rather
than from its top-left corner.

The pull-down is deliberately not pull-to-refresh. That gesture means "fetch again" in every other
app, and here the answer to a pull is a list of notes. Refreshing happens on its own: the vault is
walked again every time the app comes to the foreground.

## Chrome

The desktop fades its chrome while the reader types. A phone has almost none to fade, so the rule
becomes: the bar above a note goes when the content scrolls down and comes back when it scrolls up
or is tapped, and the keyboard appearing hides it outright. Never hide the content, a message or
anything holding a decision.

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
