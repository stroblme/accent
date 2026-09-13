package io.github.stroblme.accent.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.Open
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.FileKind
import io.github.stroblme.accent.ffi.Phase
import io.github.stroblme.accent.ffi.FileRow
import kotlinx.coroutines.launch
import java.io.File

/**
 * The vault: a drawer of files with a search field on top, and whatever is open beside it.
 *
 * One pane, not a split — a phone has room for the note or for the way to another note, never
 * both. The drawer is the desktop's sidebar with its panes collapsed into one: the field
 * searches, and what it is not searching for is the tree.
 */
@Composable
fun HomeScreen(model: VaultModel) {
    val state by model.state.collectAsState()
    val drawer = rememberDrawerState(DrawerValue.Closed)
    val scope = rememberCoroutineScope()
    var switcher by remember { mutableStateOf(false) }
    val snackbar = remember { SnackbarHostState() }

    // Back undoes the last thing that opened, in the order it opened: the switcher, the drawer,
    // then whatever is being read. Only with nothing left does it leave the app.
    BackHandler(enabled = switcher) { switcher = false }
    BackHandler(enabled = !switcher && drawer.isOpen) { scope.launch { drawer.close() } }
    BackHandler(enabled = !switcher && !drawer.isOpen && (state.open != null || state.pdf != null)) {
        model.close()
    }

    state.message?.let { message ->
        LaunchedEffect(message) {
            snackbar.showSnackbar(message)
            model.said(null)
        }
    }

    ModalNavigationDrawer(
        drawerState = drawer,
        drawerContent = {
            ModalDrawerSheet(drawerContainerColor = MaterialTheme.colorScheme.surface) {
                FilesDrawer(model, state.children, state.expanded, state.results) { rel ->
                    model.openFile(rel)
                    scope.launch { drawer.close() }
                }
            }
        },
    ) {
        Scaffold(
            snackbarHost = { SnackbarHost(snackbar) },
            contentWindowInsets = WindowInsets.safeDrawing,
        ) { padding ->
            Box(Modifier.fillMaxSize().padding(padding)) {
                when {
                    state.pdf != null -> PdfScreen(path = state.pdf!!)
                    state.open != null -> NoteScreen(
                        model = model,
                        open = state.open!!,
                        onMenu = { scope.launch { drawer.open() } },
                        onPull = { switcher = true },
                    )
                    else -> Empty(
                        indexing = state.indexing,
                        scanned = state.scanned,
                        listing = state.phase == Phase.SCAN,
                        onBrowse = { scope.launch { drawer.open() } },
                    )
                }
            }
        }
    }

    if (switcher) {
        SwitcherSheet(model) { switcher = false }
    }
}

@Composable
private fun Empty(indexing: Boolean, scanned: Long, listing: Boolean, onBrowse: () -> Unit) {
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
                else -> "Pick a note, or pull down anywhere to search."
            },
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        FilledTonalButton(onClick = onBrowse) { Text("Browse files") }
    }
}

/** The search field, and under it either its results or the tree. */
@Composable
private fun FilesDrawer(
    model: VaultModel,
    children: Map<String, List<FileRow>>,
    expanded: Set<String>,
    results: List<io.github.stroblme.accent.ffi.SearchHit>,
    onOpen: (String) -> Unit,
) {
    var query by remember { mutableStateOf("") }
    Column(Modifier.fillMaxSize()) {
        OutlinedTextField(
            value = query,
            onValueChange = {
                query = it
                model.search(it)
            },
            placeholder = { Text("Search notes") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().padding(Gutter),
        )
        if (query.isBlank()) {
            LazyColumn(Modifier.fillMaxSize()) {
                rows(children, expanded, "", 0, model, onOpen)
            }
        } else {
            LazyColumn(Modifier.fillMaxSize()) {
                items(results, key = { it.relPath }) { hit ->
                    ListItem(
                        headlineContent = { Text(hit.title ?: File(hit.relPath).name) },
                        supportingContent = {
                            Text(hit.snippet, maxLines = 2, overflow = TextOverflow.Ellipsis)
                        },
                        colors = flatRow(),
                        modifier = Modifier.row { onOpen(hit.relPath) },
                    )
                }
            }
        }
    }
}

/** One directory's children, and recursively those of any that are open. */
private fun androidx.compose.foundation.lazy.LazyListScope.rows(
    children: Map<String, List<FileRow>>,
    expanded: Set<String>,
    dir: String,
    depth: Int,
    model: VaultModel,
    onOpen: (String) -> Unit,
) {
    val here = children[dir].orEmpty()
    for (row in here) {
        item(key = row.relPath) {
            val open = row.relPath in expanded
            ListItem(
                headlineContent = {
                    Text(
                        File(row.relPath).name,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        fontWeight = if (row.kind == FileKind.DIR) FontWeight.Medium else null,
                    )
                },
                leadingContent = {
                    Text(
                        when {
                            row.kind == FileKind.DIR && open -> "▾"
                            row.kind == FileKind.DIR -> "▸"
                            row.kind == FileKind.PDF -> "◆"
                            else -> "·"
                        },
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                },
                colors = flatRow(),
                modifier = Modifier
                    .padding(start = (depth * 12).dp)
                    .row {
                        if (row.kind == FileKind.DIR) model.toggle(row.relPath) else onOpen(row.relPath)
                    },
            )
        }
        if (row.kind == FileKind.DIR && row.relPath in expanded) {
            rows(children, expanded, row.relPath, depth + 1, model, onOpen)
        }
    }
}
