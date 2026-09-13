package io.github.stroblme.accent.ui

import androidx.activity.compose.BackHandler
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.Phase

/**
 * Which of the three the screen is showing.
 *
 * No stack and no navigation graph: a phone holds one thing at a time, Browse and Launch are both
 * one step from what is being read, and Back is the way out of either.
 */
private enum class Screen { Home, Browse, Launch }

/**
 * The vault: whatever is open, and the two buttons that reach everything else.
 *
 * Browse is the file tree, Launch the switcher. Both are screens rather than a drawer and a sheet,
 * because an edge swipe and a pull are gestures nothing announces, and every gesture here has to
 * have a visible twin (MOBILE_DESIGN.md).
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
                )
            }
            Buttons(
                visible = screen == Screen.Home && chrome.shown,
                onBrowse = { screen = Screen.Browse },
                onLaunch = { screen = Screen.Launch },
                modifier = Modifier.align(Alignment.BottomCenter),
            )

            when (screen) {
                Screen.Browse -> BrowseScreen(
                    model = model,
                    children = state.children,
                    expanded = state.expanded,
                    results = state.results,
                    onOpen = { rel ->
                        model.openFile(rel)
                        screen = Screen.Home
                    },
                    onClose = { screen = Screen.Home },
                )
                Screen.Launch -> SwitcherScreen(model) { screen = Screen.Home }
                Screen.Home -> Unit
            }
        }
    }
}

/**
 * Browse and Launch, floating over whatever is being read.
 *
 * They fade with the rest of the chrome, and they go outright while the keyboard is up: there is
 * no room for them there, and a keyboard means the reader is writing rather than looking for
 * something else to read.
 */
@Composable
private fun Buttons(
    visible: Boolean,
    onBrowse: () -> Unit,
    onLaunch: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val typing = WindowInsets.ime.getBottom(LocalDensity.current) > 0
    AnimatedVisibility(
        visible = visible && !typing,
        enter = fadeIn(),
        exit = fadeOut(),
        modifier = modifier.padding(bottom = 24.dp),
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Pill("Browse", onBrowse)
            Pill("Launch", onLaunch)
        }
    }
}

@Composable
private fun Empty(indexing: Boolean, scanned: Long, listing: Boolean) {
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
                else -> "Browse your files, or Launch straight to a note."
            },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
