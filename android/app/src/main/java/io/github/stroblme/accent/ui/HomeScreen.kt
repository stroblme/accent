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
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.Phase

/**
 * Which of the two the screen is showing.
 *
 * No stack and no navigation graph: a phone holds one thing at a time, Browse is one step from
 * what is being read, and Back is the way out of it.
 */
private enum class Screen { Home, Browse }

/**
 * The vault: whatever is open, and the one button that reaches everything else.
 *
 * Browse is the files, the search and the palette behind one set of chips. A screen rather than a
 * drawer or a sheet, because an edge swipe and a pull are gestures nothing announces, and every
 * gesture here has to have a visible twin (MOBILE_DESIGN.md).
 */
@Composable
fun HomeScreen(model: VaultModel) {
    val state by model.state.collectAsState()
    var screen by remember { mutableStateOf(Screen.Home) }
    val chrome = remember { Chrome() }
    val snackbar = remember { SnackbarHostState() }

    // Whatever opens arrives with its chrome up, however the last thing read was left.
    LaunchedEffect(state.open?.rel, state.pdf) { chrome.show() }

    // Back undoes the last thing that opened, in the order it opened: the screen over the note,
    // then the note. Only with nothing left does it leave the app.
    BackHandler(enabled = screen != Screen.Home) { screen = Screen.Home }
    BackHandler(enabled = screen == Screen.Home && (state.open != null || state.pdf != null)) {
        model.close()
    }

    state.message?.let { message ->
        LaunchedEffect(message) {
            snackbar.showSnackbar(message)
            model.said(null)
        }
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        contentWindowInsets = WindowInsets.safeDrawing,
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            // What is being read stays composed under the other two rather than being swapped out
            // for them: a note's rendered view is a `WebView`, and one built again is one scrolled
            // back to the top. Closing Browse is coming back to the same line.
            val pdf = state.pdf
            val open = state.open
            when {
                pdf != null -> PdfScreen(path = pdf, chrome = chrome)
                open != null -> NoteScreen(
                    model = model,
                    open = open,
                    root = state.root.orEmpty(),
                    chrome = chrome,
                )
                else -> Empty(
                    indexing = state.indexing,
                    scanned = state.scanned,
                    listing = state.phase == Phase.SCAN,
                    onCloseVault = { model.closeVault() },
                )
            }
            BrowseButton(
                // Not while the note's own find is open: that bar has the foot of the screen, and
                // a pill over it would offer the vault's search to a reader already searching the
                // page in front.
                visible = screen == Screen.Home && chrome.shown && open?.finding != true,
                onBrowse = { screen = Screen.Browse },
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
                BrowseScreen(
                    model = model,
                    children = state.children,
                    expanded = state.expanded,
                    results = state.results,
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

/**
 * Browse, floating over whatever is being read.
 *
 * It fades with the rest of the chrome, and goes outright while the keyboard is up: there is no
 * room for it there, and a keyboard means the reader is writing rather than looking for something
 * else to read.
 */
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

/**
 * The screen with nothing open: what the vault is doing, and the way back out of it.
 *
 * A first index is watched from here, so it says in words how far the walk has got and draws the
 * platform's own indeterminate bar under them. Indeterminate because the walk has no total until
 * it ends, and a percentage nobody can honour is worse than a bar that only says "still going".
 *
 * Closing the vault belongs here and nowhere else. It is the one thing that makes sense with no
 * note in front of the reader, and a control that reached over an open document would be a way of
 * losing one's place by mistake.
 */
@Composable
private fun Empty(
    indexing: Boolean,
    scanned: Long,
    listing: Boolean,
    onCloseVault: () -> Unit,
) {
    Column(
        Modifier.fillMaxSize().padding(Gutter),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            if (indexing) "Reading your vault…" else "Nothing open",
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            when {
                // A first walk of a large vault takes minutes over shared storage, so it says how
                // far it has got. Nothing can be opened while it is still finding the files;
                // once it starts reading them, what it has read is already there to open.
                listing && scanned > 0 -> "Found %,d files so far.".format(scanned)
                listing -> "Looking through your files."
                indexing && scanned > 0 -> "%,d files read. You can start now.".format(scanned)
                indexing -> "Reading what it found."
                else -> "Browse your files, or search straight to a note."
            },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        // The space is kept whether or not there is a bar in it, so the lines above stay where
        // they are when the walk ends rather than settling half its height downwards.
        Box(Modifier.height(ProgressHeight), contentAlignment = Alignment.Center) {
            if (indexing || listing) LinearProgressIndicator(Modifier.width(ProgressWidth))
        }
        Spacer(Modifier.height(24.dp))
        TextButton(onClick = onCloseVault) { Text("Close vault") }
    }
}
