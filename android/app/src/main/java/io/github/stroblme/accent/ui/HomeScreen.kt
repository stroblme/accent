package io.github.stroblme.accent.ui

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.Open
import io.github.stroblme.accent.OpenImage
import io.github.stroblme.accent.OpenPdf
import io.github.stroblme.accent.VaultModel

/** Current vault screen: Browse or one open document. */
private enum class Screen { Home, Browse }

/** Show the current document with Browse as the route to files, search, and commands. */
@Composable
fun HomeScreen(model: VaultModel) {
    val state by model.state.collectAsState()
    var screen by remember { mutableStateOf(Screen.Home) }
    // The tag a tap in a note asked Browse to open on; the button opens it on Search.
    var browseTag by remember { mutableStateOf<String?>(null) }
    val chrome = remember { Chrome() }
    val snackbar = remember { SnackbarHostState() }

    // Whatever opens arrives with its chrome up, however the last thing read was left.
    LaunchedEffect(state.doc?.rel) { chrome.show() }

    // Back undoes the last thing that opened, in the order it opened: the screen over the note
    // (Browse's handler is its own, below), then the note. Only with nothing left does it leave
    // the app.
    val reading = state.doc != null
    BackHandler(enabled = screen == Screen.Home && reading) {
        model.close()
    }

    state.message?.let { message ->
        LaunchedEffect(message) {
            snackbar.showSnackbar(message)
            model.said(null)
        }
    }

    Scaffold(
        // Above everything, as long as it lasts: the vault on screen has stopped being read.
        topBar = { if (state.gone) GoneBanner(onReopen = { model.reopen() }) },
        snackbarHost = { SnackbarHost(snackbar) },
        contentWindowInsets = WindowInsets.safeDrawing,
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            // What is being read stays composed under the other two rather than being swapped out
            // for them: a note's rendered view is a `WebView`, and one built again is one scrolled
            // back to the top. Closing Browse is coming back to the same line.
            val doc = state.doc
            when (doc) {
                is OpenPdf -> PdfScreen(
                    model = model,
                    pdf = doc,
                    indexing = state.indexing,
                    chrome = chrome,
                )
                is Open -> NoteScreen(
                    model = model,
                    open = doc,
                    chrome = chrome,
                    onTag = {
                        browseTag = it
                        screen = Screen.Browse
                    },
                )
                is OpenImage -> ImageScreen(image = doc, chrome = chrome)
                null -> Empty(
                    opening = state.opening,
                    indexing = state.indexing,
                    paused = state.paused,
                    scanned = state.scanned,
                    ready = state.ready,
                    onStop = { model.stopIndexing() },
                    onResume = { model.resumeIndexing() },
                    onCloseVault = { model.closeVault() },
                )
            }
            BrowseButton(
                // Not while the document's own find is open: that bar has the foot of the screen,
                // and a pill over it would offer the vault's search to a reader already searching
                // the page in front.
                //
                // And not until there is something to browse. A button that would open an empty
                // tree, an empty switcher and a search with nothing to search is a control that
                // can do nothing, which is never shown (DESIGN.md Principle 1) — greyed out least
                // of all, since the line above it is already saying what the vault is doing. It
                // reads [VaultState.ready], which is the same state that line reads, so the two
                // cannot disagree about whether the reader may start.
                visible = screen == Screen.Home && chrome.shown &&
                    (doc as? Open)?.finding != true && (doc as? OpenPdf)?.finding != true &&
                    state.ready,
                onBrowse = {
                    browseTag = null
                    screen = Screen.Browse
                },
                modifier = Modifier.align(Alignment.BottomCenter),
            )

            // Browse comes up from the foot of the screen, where its field and its chips are, and
            // goes back down the way a pull sends it: the gesture and the transition say the same
            // thing. It fades as it travels, so the note it is covering is briefly visible through
            // it and the panel reads as being over the note rather than instead of it.
            AnimatedVisibility(
                visible = screen == Screen.Browse,
                enter = slideInVertically(arriving()) { it } + fadeIn(arriving()),
                exit = slideOutVertically(leaving()) { it } + fadeOut(leaving()),
            ) {
                // Composed with the panel rather than up top: the handler composed last is asked
                // first, and a PDF composes one of its own (Back through its links) after this
                // screen has.
                BackHandler(enabled = screen == Screen.Browse) { screen = Screen.Home }
                BrowseScreen(
                    model = model,
                    children = state.children,
                    expanded = state.expanded,
                    results = state.results,
                    searched = state.searched,
                    searchFailed = state.searchFailed,
                    revision = state.revision,
                    indexing = state.indexing,
                    paused = state.paused,
                    scanned = state.scanned,
                    front = doc?.rel,
                    openTag = browseTag,
                    printable = doc is Open || doc is OpenPdf,
                    onOpen = { rel, find ->
                        model.openFile(rel, find)
                        screen = Screen.Home
                    },
                    onClose = { screen = Screen.Home },
                )
            }
        }
    }
}

/** Say the vault is no longer read, with the one way back. */
@Composable
private fun GoneBanner(onReopen: () -> Unit) {
    // The top bar's slot takes no insets of its own, so the banner keeps clear of the status bar.
    val clear = WindowInsets.safeDrawing.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal)
    Column(Modifier.fillMaxWidth().windowInsetsPadding(clear)) {
        Column(Modifier.padding(horizontal = Gutter, vertical = 8.dp)) {
            Text(
                "This vault is no longer read. Changes made elsewhere will not show until it " +
                    "reopens.",
                style = MaterialTheme.typography.bodyMedium,
            )
            TextButton(onClick = onReopen) { Text("Reopen") }
        }
        HorizontalDivider()
    }
}

/** Show Browse over the document chrome and hide it while the keyboard is open. */
@Composable
private fun BrowseButton(
    visible: Boolean,
    onBrowse: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val typing = WindowInsets.ime.getBottom(LocalDensity.current) > 0
    AnimatedVisibility(
        visible = visible && !typing,
        enter = fadeIn(),
        exit = fadeOut(),
        modifier = modifier.padding(bottom = 24.dp),
    ) {
        Pill("Browse", onBrowse)
    }
}

/** The bar under the text: as wide as a word, as tall as Material draws its own track. */
private val ProgressWidth: Dp = 160.dp
private val ProgressHeight: Dp = 4.dp

/** Show indexing progress and controls when no document is open. */
@Composable
private fun Empty(
    opening: Boolean,
    indexing: Boolean,
    paused: Boolean,
    scanned: Long,
    ready: Boolean,
    onStop: () -> Unit,
    onResume: () -> Unit,
    onCloseVault: () -> Unit,
) {
    Column(
        Modifier.fillMaxSize().padding(Gutter),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            when {
                opening || indexing -> "Reading your vault…"
                paused -> "Reading paused"
                else -> "Nothing open"
            },
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            when {
                // Ahead of the lines below, which are about a walk still running: a stopped one
                // may have read nothing at all.
                paused && ready -> "You can browse files read so far."
                paused -> "No files read yet. Resume to continue."
                // A first walk of a large vault takes minutes over shared storage, so it says how
                // far it has got. Nothing can be opened while it is still finding the files;
                // once it starts reading them, what it has read is already there to open — which
                // is [ready], and is the same moment the Browse button turns up.
                !ready && scanned > 0 -> "Found %,d files so far.".format(scanned)
                !ready -> "Looking through your files…"
                indexing && scanned > 0 -> "%,d files read. You can start now.".format(scanned)
                // A walk over an index that already has files: the rescan a resume starts.
                indexing -> "Reading your files…"
                else -> "Your notes are ready to browse."
            },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        // The space is kept whether or not there is a bar in it, so the lines above stay where
        // they are when the walk ends rather than settling half its height downwards.
        Box(Modifier.height(ProgressHeight), contentAlignment = Alignment.Center) {
            if (opening || indexing) LinearProgressIndicator(Modifier.width(ProgressWidth))
        }
        Spacer(Modifier.height(24.dp))
        // One row, so the lines above stay put when either control comes or goes. No Stop while
        // the vault is opening: there is no walk yet for it to stop.
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (indexing) TextButton(onClick = onStop) { Text("Stop") }
            if (paused) TextButton(onClick = onResume) { Text("Resume") }
            TextButton(onClick = onCloseVault) { Text("Close vault") }
        }
    }
}
