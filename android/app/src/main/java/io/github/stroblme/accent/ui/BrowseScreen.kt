package io.github.stroblme.accent.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.Corpus
import io.github.stroblme.accent.Recents
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.FileKind
import io.github.stroblme.accent.ffi.FileRow
import io.github.stroblme.accent.ffi.SearchHit
import io.github.stroblme.accent.ffi.fuzzyRank
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

/** What the query means. The rest of the screen is the same in all three. */
private enum class Mode { Search, Files, Command }

/**
 * The vault's files, on a screen of their own.
 *
 * The desktop's eight sidebar panes and its palette collapse into one list: what the field means is
 * a chip away. Search reads the files and falls back to the tree while there is nothing to search
 * for; Files matches names against the recent ones, and Command against what the palette can do.
 * A screen rather than a drawer because a drawer is an edge swipe, and a gesture nothing announces
 * is a gesture nobody finds.
 *
 * Files and Command lay their rows out from the bottom up, so the best match is the one nearest
 * the field and the thumb. Search keeps the tree the way a tree reads, from the top.
 *
 * A row hands over the file it names and, if it was a search hit, the query it was found by:
 * that query is the whole difference between the two ways in, and it is what the note marks
 * itself with (`NoteScreen`). A row picked off the tree or the switcher hands over nothing and
 * so lands at the top of its note, which is where a file somebody chose by name belongs.
 *
 * The three are pages of one pager rather than three states of one list, so the swipe between them
 * is the platform's own: the surface follows the thumb and settles at the speed it was thrown, and
 * the drag is claimed by the direction it is going in rather than by whichever node saw the finger
 * first — a page turn and a scroll down cannot be taken for each other, and neither can steal the
 * pull that closes the panel. The chips remain the control and the only announcement: tapping one
 * scrolls the pager, so it still travels the way the ordinal moved.
 */
@Composable
fun BrowseScreen(
    model: VaultModel,
    children: Map<String, List<FileRow>>,
    expanded: Set<String>,
    results: List<SearchHit>,
    onOpen: (rel: String, find: String?) -> Unit,
    onClose: () -> Unit,
) {
    val pager = rememberPagerState { Mode.entries.size }
    val scope = rememberCoroutineScope()
    // Which page the pager is nearest, which is what the field means and which chip is lit: both
    // follow the surface across, rather than waiting for it to land.
    val mode = Mode.entries[pager.currentPage]
    var query by remember { mutableStateOf("") }
    // The ranking and the page it was ranked for, as places in [Commands] or in [files]. Two pages
    // are composed at once while one is being dragged in, and a file row drawn from a command's
    // place is a row that says the wrong thing.
    var ranked by remember { mutableStateOf(Mode.Search to emptyList<Int>()) }
    // What the Files page's places are places in.
    var files by remember { mutableStateOf(Corpus()) }
    val focus = remember { FocusRequester() }

    LaunchedEffect(query, mode) {
        if (mode == Mode.Search) return@LaunchedEffect
        val commands = mode == Mode.Command
        val kind = if (commands) Recents.Kind.Commands else Recents.Kind.Notes
        // The note corpus is fetched on demand rather than kept in step with the index, so the
        // first query in a session waits for it once.
        val read = if (commands) null else model.corpus()
        val corpus = read?.names ?: Commands.map { it.label }
        ranked = mode to withContext(Dispatchers.Default) {
            if (read != null && query.isBlank()) {
                model.recents.list(Recents.Kind.Notes).mapNotNull { read.find(it) }.take(50)
            } else {
                val ranks = model.recents.ranks(kind, corpus)
                fuzzyRank(query, corpus, ranks).map { it.toInt() }
            }
        }
        // With the places it holds, in the same frame.
        if (read != null) files = read
    }

    // Reaching for a chip is reaching for the keyboard. Opening the screen is not: Search lands on
    // the tree, and a keyboard over it would be half the screen spent on nothing. This one waits
    // for the page to settle — half the screen taken away mid-swipe is taken away from a gesture
    // that has not finished.
    val landed = Mode.entries[pager.settledPage]
    LaunchedEffect(landed) { if (landed != Mode.Search) focus.requestFocus() }

    PullDownPanel(onClose) {
        ScreenBar("Browse")
        HorizontalPager(state = pager, modifier = Modifier.weight(1f).fillMaxWidth()) { page ->
            val tab = Mode.entries[page]
            val rows = if (ranked.first == tab) ranked.second else emptyList()
            LazyColumn(Modifier.fillMaxSize(), reverseLayout = tab != Mode.Search) {
                when {
                    tab == Mode.Command -> items(rows, key = { it }) { i ->
                        val command = Commands[i]
                        ListItem(
                            headlineContent = {
                                Text(command.label, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            },
                            colors = flatRow(),
                            modifier = Modifier.row {
                                model.recents.touch(Recents.Kind.Commands, command.label)
                                command.run(model)
                                onClose()
                            },
                        )
                    }
                    tab == Mode.Files -> items(rows, key = { it }) { i ->
                        val row = files.row(i)
                        ListItem(
                            headlineContent = {
                                Text(row.name, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            },
                            supportingContent = {
                                Text(row.rel, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                            },
                            // At the row's end, what the note is not: the name alone reads as a
                            // file that is there.
                            trailingContent = if (!row.unwritten) null else ({
                                Text(
                                    "Not created",
                                    style = MaterialTheme.typography.labelMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }),
                            colors = flatRow(),
                            modifier = Modifier.row {
                                if (row.unwritten) {
                                    model.create(row.rel)
                                    onClose()
                                } else {
                                    onOpen(row.rel, null)
                                }
                            },
                        )
                    }
                    query.isBlank() -> rows(children, expanded, "", 0, model, onOpen)
                    // A file answers with one hit per occurrence, so the path alone is not a key:
                    // where in the file the hit sits is what tells two rows of one note apart.
                    else -> items(results, key = { "${it.relPath}:${it.at?.start}" }) { hit ->
                        ListItem(
                            headlineContent = { Text(hit.title ?: File(hit.relPath).name) },
                            supportingContent = {
                                Text(hit.snippet, maxLines = 2, overflow = TextOverflow.Ellipsis)
                            },
                            colors = flatRow(),
                            modifier = Modifier.row { onOpen(hit.relPath, query) },
                        )
                    }
                }
            }
        }
        Row(
            Modifier.padding(horizontal = Gutter),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            for (choice in Mode.entries) {
                FilterChip(
                    selected = mode == choice,
                    // The tap travels the same distance the thumb would have dragged, at the
                    // sideways token; a swipe settles on its own velocity, as a thrown page should.
                    onClick = {
                        scope.launch {
                            pager.animateScrollToPage(choice.ordinal, animationSpec = stepping())
                        }
                    },
                    label = { Text(choice.name) },
                )
            }
        }
        Field(
            value = query,
            onValue = { query = it; if (mode == Mode.Search) model.search(it) },
            placeholder = when (mode) {
                Mode.Search -> "Search"
                Mode.Files -> "Go to a file"
                Mode.Command -> "Run a command"
            },
            modifier = Modifier.focusRequester(focus),
        )
    }
}

/** One directory's children, and recursively those of any that are open. */
private fun LazyListScope.rows(
    children: Map<String, List<FileRow>>,
    expanded: Set<String>,
    dir: String,
    depth: Int,
    model: VaultModel,
    onOpen: (rel: String, find: String?) -> Unit,
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
                        if (row.kind == FileKind.DIR) {
                            model.toggle(row.relPath)
                        } else {
                            onOpen(row.relPath, null)
                        }
                    },
            )
        }
        if (row.kind == FileKind.DIR && row.relPath in expanded) {
            rows(children, expanded, row.relPath, depth + 1, model, onOpen)
        }
    }
}

/** What the palette can do here. Everything a phone has no place for is simply not in the list. */
class Command(val label: String, val run: (VaultModel) -> Unit)

/**
 * Find is the palette's one entry that changes the screen rather than the vault: it opens the find
 * bar of the note or the PDF in front, which searches that where Search above searches every
 * note. Close Vault was reachable only from the screen with nothing open,
 * which is the one place a reader is not thinking about the vault they are in.
 */
val Commands = listOf(
    Command("New Note") { it.newNote(newName()) },
    Command("Find") { it.finding(true) },
    Command("Reload Vault") { it.reloadVault() },
    Command("Close Note") { it.close() },
    Command("Close Vault") { it.closeVault() },
)

private fun newName(): String {
    val stamp = java.text.SimpleDateFormat("yyyy-MM-dd HHmm", java.util.Locale.US)
        .format(java.util.Date())
    return "Untitled $stamp.md"
}
