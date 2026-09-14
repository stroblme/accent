package io.github.stroblme.accent.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.Recents
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.FileKind
import io.github.stroblme.accent.ffi.FileRow
import io.github.stroblme.accent.ffi.SearchHit
import io.github.stroblme.accent.ffi.fuzzyRank
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File

/** What the query means. The rest of the screen is the same in all three. */
private enum class Mode { Search, Files, Command }

/**
 * The vault's files, on a screen of their own.
 *
 * The desktop's eight sidebar panes and its palette collapse into one list: what the field means is
 * a chip away. Search reads the notes and falls back to the tree while there is nothing to search
 * for; Files matches names against the recent ones, and Command against what the palette can do.
 * A screen rather than a drawer because a drawer is an edge swipe, and a gesture nothing announces
 * is a gesture nobody finds.
 *
 * Files and Command lay their rows out from the bottom up, so the best match is the one nearest
 * the field and the thumb. Search keeps the tree the way a tree reads, from the top.
 */
@Composable
fun BrowseScreen(
    model: VaultModel,
    children: Map<String, List<FileRow>>,
    expanded: Set<String>,
    results: List<SearchHit>,
    onOpen: (String) -> Unit,
    onClose: () -> Unit,
) {
    var mode by remember { mutableStateOf(Mode.Search) }
    var query by remember { mutableStateOf("") }
    var ranked by remember { mutableStateOf<List<String>>(emptyList()) }
    val focus = remember { FocusRequester() }

    LaunchedEffect(query, mode) {
        if (mode == Mode.Search) return@LaunchedEffect
        val commands = mode == Mode.Command
        val kind = if (commands) Recents.Kind.Commands else Recents.Kind.Notes
        // The note corpus is fetched on demand rather than kept in step with the index, so the
        // first query in a session waits for it once.
        val corpus = if (commands) Commands.map { it.label } else model.corpus()
        ranked = withContext(Dispatchers.Default) {
            if (query.isBlank() && !commands) {
                model.recents.list(Recents.Kind.Notes).filter { it in corpus }.take(50)
            } else {
                val ranks = model.recents.ranks(kind, corpus)
                fuzzyRank(query, corpus, ranks).map { corpus[it.toInt()] }
            }
        }
    }

    // Reaching for a chip is reaching for the keyboard. Opening the screen is not: Search lands on
    // the tree, and a keyboard over it would be half the screen spent on nothing.
    LaunchedEffect(mode) { if (mode != Mode.Search) focus.requestFocus() }

    PullDownPanel(onClose) {
        ScreenBar("Browse")
        LazyColumn(
            Modifier.weight(1f).fillMaxWidth(),
            reverseLayout = mode != Mode.Search,
        ) {
            when {
                mode != Mode.Search -> items(ranked, key = { it }) { row ->
                    ListItem(
                        headlineContent = {
                            Text(
                                if (mode == Mode.Command) row else File(row).name,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        },
                        supportingContent = if (mode == Mode.Command) null else ({
                            Text(row, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                        }),
                        colors = flatRow(),
                        modifier = Modifier.row {
                            if (mode == Mode.Command) {
                                model.recents.touch(Recents.Kind.Commands, row)
                                Commands.first { it.label == row }.run(model)
                                onClose()
                            } else {
                                onOpen(row)
                            }
                        },
                    )
                }
                query.isBlank() -> rows(children, expanded, "", 0, model, onOpen)
                else -> items(results, key = { it.relPath }) { hit ->
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
        Row(
            Modifier.padding(horizontal = Gutter),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            for (choice in Mode.entries) {
                FilterChip(
                    selected = mode == choice,
                    onClick = { mode = choice },
                    label = { Text(choice.name) },
                )
            }
        }
        Field(
            value = query,
            onValue = { query = it; if (mode == Mode.Search) model.search(it) },
            placeholder = when (mode) {
                Mode.Search -> "Search notes"
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

/** What the palette can do here. Everything a phone has no place for is simply not in the list. */
class Command(val label: String, val run: (VaultModel) -> Unit)

val Commands = listOf(
    Command("New Note") { it.newNote(newName()) },
    Command("Reload Vault") { it.rescan() },
    Command("Close Note") { it.close() },
)

private fun newName(): String {
    val stamp = java.text.SimpleDateFormat("yyyy-MM-dd HHmm", java.util.Locale.US)
        .format(java.util.Date())
    return "Untitled $stamp.md"
}
