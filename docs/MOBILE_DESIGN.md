# accent on Android — design rules

The [desktop principles](DESIGN.md) also apply on Android; this document covers phone-specific decisions.
Follow these rules where they differ from Material 3, using a flat layout and typographic hierarchy.

## Principles, restated for a phone

1. **One thing at a time:** show one document or its navigation, without splits or tabs.
2. **Flat pages:** use rows and sheets without cards, elevation or tonal containers; `ui/Theme.kt` maps container colours to the surface.
3. **Typographic hierarchy:** use size and weight for emphasis and the system accent for interaction.
4. **Touch for navigation:** reserve drawing for a stylus when PDF annotation tools are enabled.
5. **Reading by default:** open notes rendered and enter editing explicitly.

## Layout

### Vault picker

- A centred column: the name, one line of explanation, the permission if it is missing, one button, then the recent vaults as plain rows.
- A pick leaves it on the next frame, for the vault's own screen reading "Reading your vault…" over the indeterminate bar while the core is still opening the index; a folder that cannot be opened comes back here and says why in a `Snackbar`.
  A picker that stays put after a tap reads as a tap that was lost.

### Browse

- A panel over what is being read, with the Search / Files / Tags / Backlinks / Command chips and the query field at the foot of the screen, nearest the thumb.
  The desktop's eight sidebar panes and its palette collapse into it.
- The chips go from the whole vault to the one document and then to acting on it: what the notes say, what they are called, what they are about, what links to the one in front, what can be done.
  Five do not fit across a phone, so the row scrolls sideways under the thumb and keeps the lit chip on screen as the pages are swiped; the chip cut off at the edge is what says there is more.
  A scrolling row rather than shorter labels or icons, because each is the one word for its page and the platform's own chips scroll the same way.
- Search is the default: the tree until there is a query, then what the notes say.
  Files is the switcher — the recent files, then names ranked against the query — and Command the palette; both lay their rows out from the bottom up, so the best match is nearest the thumb, and both put the keyboard up when their page lands.
  Files lists the index's names at once, and the notes in the folders git ignores join when their walk lands, which over shared storage is a walk of the whole vault; the names are ranked on the core's side, so a key sends the query and not them.
- Every list here is the index's, so while the vault is still being read a muted line under the title says what is not listed yet, over the start screen's bar; a stopped walk keeps the line and drops the bar.
  Search and Files ask again as the walk reads more files, so a file it reaches turns up without the query being typed again, and a query that matched nothing says "Nothing found yet" until it ends.
  A query unanswered after 200 ms puts the bar up as well.
- A search hit opens its note marked by the page's own find; where that find cannot see what the index matched — an accent folded, markup the page does not show, a title — the note opens unmarked at its top rather than marked at the wrong words.
- New from Template… and Today's Note are the commands that ask first: the Command page lists the templates they offer under the command's name in the accent, as a tag lists its notes, and the field narrows them.
  A pick makes the note there, or opens the one it made — a dated target asked twice in a day is the day's note — and Back returns to the commands.
  New from Template… offers every template that says where its notes go (`accent-target:`, as the desktop's does); Today's Note those whose destination is dated (`{{date…}}`), and goes straight to the day's note when there is one.
  With none to offer, either says so in a `Snackbar` instead.
- Tags is every tag with how many notes carry it, most used first and bottom-up as Files is, the query ranking them as it ranks names.
  A tap opens the tag on the same page: its notes by name and folder, from the top, under `#tag · N` in the accent, which a tap or Back takes back to the tags.
  A tag tapped in a note — a link in the accent, as the editor colours it — opens Browse on that page, its notes listed, as if it had been opened here.
- Backlinks is the notes linking to the document in front — a note, a PDF or an image — once each, by name and folder under "Links to …", the desktop's References for a note.
  One opened from here is one step from where the reader was: Back from it opens that document again where they left it — a note scrolled as it was, a PDF at its page, zoom and pan, an image fitted as it always opens.
  With nothing in front, or nothing linking, the page says so in a line where the rows would be.
- Tags and Backlinks keep the keyboard down when they land: each is a list to read, as the tree is, and the field is there to narrow it.
- A handle at the top, and a pull down anywhere in it closes it.
  No Close button: Back already does that, and a second way out costing a corner of the screen is a corner spent twice.

### Note

- Reading: the rendered note in a WebView, 16 dp side gutters, one thin bar over the top of it and, while a find is open, the find bar below.
  The page is loaded when the note, the palette or an image inverted in it changes, and at no other time: a WebView told to load again sends the reader back to the top, so a load of the note already in front puts them back where they were once it has been drawn, and so does Done, which builds the view again.
- A tapped link goes where the desktop preview sends it (`markdown::click`): a wikilink or a link to a vault file opens it, an anchor into the note scrolls there, a tag opens Browse on its notes, and an address the desktop hands to the system goes to whichever app opens it, on a tap only.
  A PDF's own links keep their shorter list (http, https, mailto, tel).
- Editing: the same text with the same styling spans, markup visible and dimmed.

### PDF

- A column of pages with the note's bar drawn over them, not above them: a page is read at a zoom and an offset the reader chose, and chrome that takes layout space moves both each time it comes and goes.
- Back first retraces the jumps taken inside the document — a link followed, a bookmark picked — latest first, as the desktop's history does; with none left it leaves the document.
- The highlights the vault's notes make by linking into a passage (`[[paper.pdf#page=…&selection=…]]`) are painted over the page in the accent at 20 %, always, as on the desktop.
  A tap on one opens the note that makes it, marked at the text the link quotes, and Back from that note returns to the page, the zoom and the pan it was left at.
- Following such a link from a note opens the PDF on its page with the passage selected and brought to the middle of the screen; numbers that fit no line of the page open the page.
- Other readers' comments — what a mark in the file says (`/Contents`) — are read, never written, as on the desktop: pdfium paints their marks, and a tap on one that no link and no note's highlight takes shows every comment under the finger in an `AlertDialog`, titled by its author ("Comment" with none; "Comments" over several, each under its author), the text selectable to copy.
  Close, Back or a tap outside puts it away: a comment is text asked for, read and let go.
- A long press selects the word under the finger and the drag after it grows the selection; two accent handles then move either end, across a page break as readily as within a page — the desktop's drag in the platform's shape.
  The platform's floating toolbar offers Copy and Copy link.
- Copy link puts the desktop's own link on the clipboard, `[[paper.pdf#page=N&selection=a,b,c,d|the quoted text]]`, one per page the selection covers, and pasting it into a note is what makes a highlight.
  A PDF opened from another app is named by the file name its provider gives, as the desktop names a file from outside a vault.
- A tap while text is selected lets the selection go and does nothing else.
- Find — the bar's or the palette's — searches the whole document a page at a time, from the page under the middle of the screen round to the one before it, lands on the first match at or after that page and steps round the ends; the matches are at the desktop's 30 %, the current one at 60 %.
  Its bar sits at the foot of the pages, as a note's does.
- The annotation toolbar is off (`PdfScreen.ANNOTATIONS`) until its design settles: until then no tool can be picked and a finger only ever moves the page.

### Image

- An image file — from the files, the switcher, a link, or a tap on it in a note — opens on a screen of its own rather than as a note: the image fitted whole to the screen on the page colour, under the document's bar.
  A pinch zooms it, the WebView's own.
- It is drawn by the note's rule (Colour), and the bar's one action is Invert, which sends this file to the other theme's page for as long as the app runs, as a long press on the image in a note does.
- A TIFF is not an image here (`imageKind`), unlike on the desktop: neither `BitmapFactory` nor the WebView reads one, so it opens as any other file, and an embed of one in a note, which the shared markdown makes an image, shows as one that did not load.

### Print

- Print… is a palette command, listed while a note or a PDF is in front.
  It opens the system's print UI, which paginates, picks the printer and offers Save as PDF, so there is no Export as… beside it.
- A note goes out as the desktop prints one: the text as typed, on the light page whatever the theme, links in the light theme's accent, images as their files are (no recolouring, no inversion), diagrams in mermaid's light theme.
  A WebView of its own that is never shown lays it out (`WebView.createPrintDocumentAdapter`), once its images are in and its diagrams drawn, so the page being read keeps its place and its marks.
- A PDF goes out as a copy of the document as it stands, its ink in and the notes' highlights written in as `/Highlight` annotations in that accent, each carrying the text it quotes (`PdfSession::copy_with_highlights`): the desktop's copy.
  The file itself gains nothing.

### Message

- A `Snackbar`.
  A state that needs a decision is an inline row above the content, not a dialog — except on the way out, where the row would go with the content: leaving a note over edits whose saving was paused asks the row's question in an `AlertDialog`, and over edits that could not be written (a disk error) asks whether to stay or leave without them.
- A failure says what could not be done and why, never an empty answer in its place: a list that says "nothing" when it could not be read tells the reader something false about their vault.
  Where it is said follows who asked: a list in Browse says it where its rows would be, an action the reader took says it in the `Snackbar`, and a refresh nobody asked for keeps what it had and logs why.

## Spacing and type

- Side gutter 16 dp everywhere, on every screen, at every width.
  Rows 56 dp.
- The scale is 4 / 8 / 16 / 24 / 32, and nothing between.
- Body text is the system's own size, the one the reader chose; everything else is measured against it: `headlineSmall` for a screen's title, `titleMedium` for a note's, `labelMedium` in the muted colour for anything secondary.
- Dividers are hairlines or absent: a list needs neither a box nor a rule to read as a list.
- One radius scale on the theme (`ui/Theme.kt`), 8 / 12 / 16 / 20 / 28 dp, and nothing rounds itself: Material's own starts at 4 dp, which on a flat page reads as a rectangle somebody failed to round.
  A query field is the 20 dp step, a floating button the 28 dp one, past half its height, which is what makes a pill a pill.

## Gestures

Provide a visible control for each action that otherwise depends on a gesture.

| Gesture | What it does | Its visible twin |
|---|---|---|
| Pull the panel down | Closes Browse | The handle at its top |
| Swipe across the panel | Steps between Search, Files, Tags, Backlinks and Command | The chips, which travel with it |
| Tap the content | Puts the chrome up or takes it down | — |
| Scroll on | Takes the chrome down; scrolling back brings it up | — |
| Pinch on a page | Zooms a PDF, 1× to 8× (less on a viewport wider than 4095 px), around the point between the fingers | — |
| Drag on a zoomed page | Pans it, both axes at once | — |
| Tap a highlight on a page | Opens the note whose link makes it; Back returns to the page | The highlight |
| Tap another reader's comment on a page | Shows it with its author in a dialog | The mark pdfium paints |
| Back on a tag's notes | Returns to the tags | The tag's heading |
| Tap a tag in a note | Opens Browse on that tag's notes | The tag, in the accent |
| Long press on a PDF page | Selects the word under the finger; a drag grows it, and the handles move either end | The handles and the floating toolbar it raises |
| Long press on a note | Selects the text under the finger with the WebView's own handles, as a page of prose does everywhere else on the platform | The handles it raises |
| Tap an image in a note | Opens it on its own screen, to zoom and invert; an image inside a link follows the link | The image |
| Long press on an image in a note | Inverts its colours against the recolouring rule, until the app is closed | Invert on the image's own screen, a tap away |
| Pinch on an image | Zooms it, the WebView's own | — |

- Use the WebView hit test for image taps and long presses; an image inside a link follows the link on tap, while a long press inverts a still image.
  Elsewhere, a tap toggles chrome and a long press selects text.
- Pull Browse down from a stationary list to close it; a list drag that reaches its edge must stop before a second pull can close the panel.
  Back also closes it.
- Open Browse with its floating button, which hides with the chrome or keyboard; avoid an undiscoverable edge gesture.
- Feed both axes of PDF panning to one gesture handler so diagonal drags follow the hand.
- During a pinch, scale the page layer without relaying out the column; commit the zoom on release while keeping the point beneath the fingers fixed.

## The document

- Notes, PDFs and images use the same `DocumentBar` and `DocumentGap` (`ui/Common.kt`).
- Overlay the bar on reading content (`DocumentFrame`) so showing it does not change the reader's scroll or zoom position; a tap can hide it when it covers the start of a document.
- Keep the editor's persistent bar outside its scroll area and place decision banners above it.
- The bar's button is a note's Edit and Done.
  A PDF's are Find and Contents, the document's bookmarks; on a file with none, Contents is there and disabled: a gap where a control belongs is worse than a control that says it has nothing to offer.
  A PDF's third, and an image's one, is Invert, disabled on an animation, which is never recoloured.
- Fill the Invert button with the accent while active (`BarToggle`).
- Apply window insets once: the vault screen owns them for its PDF; a PDF opened from another app owns its own.

## Chrome

- Hide the document bar and Browse button on forward scroll; restore them on reverse scroll or tap, and hide Browse while the keyboard is up.
  Share this state through `Chrome` (`ui/Common.kt`).
- Two things never fade: the bar while the editor is open, Done being the only way out of it; and the content, a message, or anything holding a decision.
- A note's find bar is the exception to the keyboard rule: it stays while the keyboard is up, the keyboard being what it is for, and the Browse button goes instead, so the vault's search and the page's find are never on screen together.
  It takes its space from the note rather than lying over it — a bar across the last lines would cover the match it just found — and Back puts it away, as Back closes the panel.
- Use 200 ms for arrivals and 150 ms for departures or sideways steps (`ui/Common.kt`), with deceleration on entry and acceleration on exit.
- Respect the platform animator scale; Compose already disables these animations when the system does.

## The launcher icon

- The desktop logo (`data/icons/logo.svg`) as an adaptive icon, its white paper the background layer.
  Its shapes are strokes, so the vector drawable carries `strokeColor` and `strokeWidth` rather than fills, and the group is scaled to half size so the whole art fits inside the circle a mask may cut.

## Colour

- Use Material You's `primary` for links, the caret, selection and default ink.
- Match the desktop page and ink: `#333338` on `#ffffff` in light, `#ebebeb` on `#1d1d20` in dark (`apps/gtk/src/theme.rs`).
  Derive secondary text and borders from that ink; keep the page free of wallpaper tint.
- The one departure is a conflict block git left in a note, whose sides the rendered view boxes as the desktop's preview does, in its green and blue mixed with the ink by its numbers (`conflictTints`): a conflict's two sides have to read as two, and as they do on the desktop.
- Use `inverseSurface` for the Browse button so it stays distinct from the page without spending another accent colour.
- A PDF is recoloured in a dark theme as on the desktop: the document's paper lands on the app's surface and its ink on the app's text, each pixel keeping its chroma, so a coloured figure stays coloured.
  Invert in its bar turns that round for the file, as an image's does below.
- Recolour document-like still images and SVGs in notes and the image screen; leave photos, animations and rasters over 64 MP untouched.
  Invert places one file on the opposite theme's page until the process ends, without changing it on disk.

## Architecture

The shared architecture is in [DESIGN.md](DESIGN.md).

- **Scope**: read the vault, make small edits, link PDF passages from notes, and open PDFs from other apps through `ACTION_VIEW` without a vault.
  PDF annotation tools remain disabled pending their toolbar design.
- **Storage**: all-files access (`MANAGE_EXTERNAL_STORAGE`) and real paths, distributed as the per-ABI APKs a tagged release carries (`release.yml`), Play optional — the Storage Access Framework is 25–50× slower and cannot see a Syncthing folder that already exists, and Play allows the permission only behind a declaration.
- **The ffi** lives in `crates/api/src/ffi/` behind the `android` feature, core types declared with `#[uniffi::remote]` — one façade, and no mirrored definitions to keep in step.
- **`android/ffi` is a cdylib of its own** — `accent-api` as one would make every desktop build link a shared object nobody loads, and the static musl server build refuses cdylibs.
- **Text offsets become UTF-16 in the ffi** — the core counts bytes and Kotlin UTF-16, and a span in the wrong unit lands beside the word.
- **A colour crosses as one `u32`** (`0xRRGGBBAA`) — uniffi carries no arrays, and it is how Android spells a colour.
- **No watcher**: use `Vault::open_unwatched` and rescan when the app resumes, unless a walk is already running or paused; inotify loses events on emulated storage.
  App writes still update the index.
- **Index storage**: keep the index in `noBackupFilesDir` so Android does not evict it as cache.
- **First index**: let the reader in while the walk commits batches; show the scan count until its first batch is written.
  Stopping pauses the walk, including resume rescans.
- **Foreground indexing**: hold an `IndexingService` of type `dataSync` while walking, with a quiet, ongoing progress notification that returns to the app when tapped.
  Stop the service when the walk ends or pauses; no notification permission is required for the service to run.
- **PDF rules**: use the shared core for highlights, links and annotation data; Compose handles gestures and drawing.
- **Settings and sessions are Kotlin's** (`SharedPreferences`) — panes, splits and zoom do not exist here; what the two apps share is the vault, and that is a path.
- **The rendered view is a `WebView`** over the core's `markdown::to_html`, with the desktop's `accent://` scheme and generated CSS — it is the only thing on the platform that draws MathML.
- **Mermaid**: serve the shared bootstrap (`vendor/mermaid/bootstrap.js`) and library at `accent://app/`, enabling JavaScript only for notes with Mermaid fences.
  A fresh Content-Security-Policy nonce allows only app scripts; keep the JavaScript interface, file access and network access off, and allow requests only for vault images and the app assets.
  Use Mermaid's strict security level so labels are sanitized and click handlers are dropped.
- **Images**: `ui/Images.kt` serves animations unchanged, classifies still rasters from a 512 px copy, applies EXIF orientation, and recolours document-like images at up to 2048 px before serving a PNG.
  SVGs receive the core's colour filter, which the note and image WebViews both render.
  Clear their shared image cache when the palette or inverted-file set changes.
- **An embed resolves as on the desktop**: the path the note spells if a file is there, otherwise where the index places the name (`Vault::asset`), inside the vault or not at all.
- **The editor is `BasicTextField(TextFieldState)`** with a styled `OutputTransformation` — the only field that is not deprecated, and `TextFieldBuffer.addStyle` is where the core's spans go.
- **Wet strokes**, currently unreachable while the annotation toolbar is disabled, use a Compose `Canvas` path committed on release.
- **minSdk 31** — where Material You reads the system colours the accent comes from.
- **The release build is minified** (`app/proguard-rules.pro`) — unminified, Compose's dex was two thirds of the APK.
  What JNA reaches by name is kept by what it is — JNA itself, every `Structure`, every `Callback`, every `native` method — rather than by package, so the bindings' own Kotlin shrinks with the rest.

## What is not here, and why

- Git, language servers, ghost text, word suggestions, remote vaults, the terminal, diagrams, splits, comparison, Replace All, the minimap, focus levels and presentation mode remain desktop features.
  Add a phone feature only when it helps read the vault, make a small edit or work with a PDF.
- Deferred rather than refused: a native Compose renderer in place of the WebView, exporting highlights, and the PDF shapes and Adjust tool.
