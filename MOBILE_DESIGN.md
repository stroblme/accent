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
| Browse | A panel over what is being read, with the Search / Files / Command chips and the query field at the *foot* of the screen. Search is the default: the tree until there is a query, then what the notes say. Files is the switcher — the recent files, then names ranked against the query — and Command the palette; both lay their rows out from the bottom up, so the best match is nearest the thumb, and both put the keyboard up when their page lands. The desktop's eight sidebar panes and its palette collapse into this one |
| Note, reading | The rendered note in a WebView, 16 dp side gutters, one thin bar above it. The page is loaded when the note or the palette changes and at no other time: a WebView told to load again is a reader sent back to the top |
| Note, editing | The same text with the same styling spans, markup visible and dimmed |
| PDF | A column of pages, under the same bar a note has and in the same rectangle. Leaving the document is Back. The annotation toolbar is off (`PdfScreen.ANNOTATIONS`) until its design settles; while it is off no tool can be picked, the bar's Edit is disabled, and a finger only ever moves the page |
| The panel | A handle at the top, and a pull down anywhere in it closes it. No Close button: Back already did that, and a second way out that costs a corner of the screen is a corner spent twice |
| Message | `Snackbar`. A state that needs a decision is an inline row above the content, not a dialog |

## Spacing and type

- Side gutter 16 dp everywhere, on every screen, at every width. Rows 56 dp.
- The scale is 4 / 8 / 16 / 24 / 32. Nothing between.
- Body text is the system's own size: that is what the reader chose. Everything else is measured
  against it — `headlineSmall` for a screen's title, `titleMedium` for a note's, `labelMedium` in
  the muted colour for anything secondary.
- Dividers are hairlines or absent. A list needs neither a box nor a rule to read as a list.
- One radius scale, on the theme (`ui/Theme.kt`), stepping 8 / 12 / 16 / 20 / 28 dp; nothing rounds
  itself. Material's own starts at 4 dp, which on a flat page reads as a rectangle somebody failed
  to round. A query field is the 20 dp step, a floating button the 28 dp one — past half its height,
  which is what makes a pill a pill.

## Gestures

Nothing is keyboard-reachable here, so a gesture is the only affordance — which means every one
of them must also exist as a visible control.

| Gesture | What it does | Its visible twin |
|---|---|---|
| Pull the panel down | Closes Browse | The handle at its top |
| Swipe across the panel | Steps between Search, Files and Command | The chips, which travel with it |
| Tap the content | Puts the chrome up or takes it down | — |
| Scroll on | Takes the chrome down; scrolling back brings it up | — |
| Pinch on a page | Zooms a PDF, 1× to 6×, around the point between the fingers | — |
| Drag on a zoomed page | Pans it, both axes at once | — |
| Long press on a note | Selects the text under the finger: the WebView's own handles, which is what a page of prose does everywhere else on the platform | The handles it raises |

A panel is closed by pulling it down, which is the one gesture here that reads as itself: the
handle at the top says a panel can be moved, and moving it down is where a panel goes. What is
inside scrolls first, so only a drag the list cannot use — one with nothing left above it — pulls
the panel, and letting go short of the threshold springs it back. Nothing about it has to be
discovered, because Back does the same thing.

A drag that began by scrolling the list stops where the list does. Reaching the top of the files is
something a reader does on the way to the first of them, and it must not also be the thing that
takes the files away — so closing is a second pull, from a standstill. The rule is the one every
sheet on the platform follows, and the reason it is felt rather than noticed.

Browse is a button, not a gesture. The files were an edge swipe and the switcher a pull from the
top, and neither had the visible twin this table asks for — a gesture nothing announces is a
gesture nobody finds. One button now, because two pills at the foot of the screen are a choice
made before the reader knows which one they want; the chips ask the same question inside, where
the answer is already on the screen. It floats at the foot of whatever is being read, goes with
the rest of the chrome, and goes outright while the keyboard is up.

A document is one surface, not a vertical scroller with a horizontal one wrapped around it. Two
scroll containers each own an axis and each claims a drag the moment it looks like theirs, which
is what makes a diagonal drag pick a side; one gesture handler feeding both axes is what makes it
follow the hand. The same handler is what lets a pinch grow the page away from the fingers rather
than from its top-left corner.

A pinch never lays the column out again. It scales one layer under the fingers and commits once,
on release. The column that layer holds is placed by its top-left corner and not by any of the
`required*` modifiers, which report the parent a size coerced back into the incoming constraints
and then centre what overflows: half of everything a zoom adds comes off the left, which is a page
that jumps sideways on every pinch and a band of background down its right. Asking the list where it is and telling it where to go in the same frame reads back
the position from before the answer, which walks the page out from under the hand a little more
every frame; and a column held to the width of the screen grows taller without growing wider,
which is a page squeezed sideways. Refreshing happens on its own: the vault is walked again every
time the app comes to the foreground.

## The document

A note and a PDF are one surface with two kinds of content in it, so they are built that way: the
same `DocumentBar` — the file's name, and the one thing that can be done to it — over the same
`DocumentGap` of clear space, with the content in the rectangle that leaves (`ui/Common.kt`).
Moving between a note and a PDF should not move what is being read.

The bar's button is a note's Edit and Done. A PDF's is the same button, disabled, for as long as
there is nothing to edit: a gap where a control belongs is worse than a control that says it is not
available yet.

Only one screen may keep window insets. A PDF inside a vault is already inside a screen that holds
itself clear of the status bar, so its own scaffold takes none; opened from another app there is no
such screen, and it keeps them itself. Applying them twice is a bar that sits lower than a note's.

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

The device is asked for the accent and for nothing else. Material You is the same answer GNOME's
accent is — what colour is this device — so `primary` comes from it, for exactly what the desktop
uses it for: links, the caret, a selected tool, an ink stroke's default. The page and the ink on it
are the app's own, and the app's own are the desktop's: `#333338` ink on `#ffffff` paper in light,
`#ebebeb` on `#1d1d20` in dark, which are libadwaita's view colours out of `apps/gtk/src/theme.rs`,
the light ink composited down from the 80% alpha it has there. A wallpaper-tinted page reads cream
beside that view, and the same vault should look like one editor on both. Secondary text and borders
are that ink thinned over that page, so nothing under the accent is tinted by anything. Those four
values are every colour the app ships; the launcher icon's paper is the light page.

The Browse button is the exception: `inverseSurface`, which is the *other* mode's pair, so it is
dark on a light theme and light on a dark one. It is the one thing on the screen that is not the
document, and a pale pill on a pale page is a pill nobody sees. Contrast rather than colour, so the
accent still means only one thing.

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
