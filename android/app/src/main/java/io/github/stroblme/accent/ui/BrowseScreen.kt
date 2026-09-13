package io.github.stroblme.accent.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.FileKind
import io.github.stroblme.accent.ffi.FileRow
import io.github.stroblme.accent.ffi.SearchHit
import java.io.File

/**
 * The vault's files, on a screen of their own.
 *
 * The desktop's eight sidebar panes collapse into one list: the field searches, and what it is not
 * searching for is the tree. A screen rather than a drawer because a drawer is an edge swipe, and
 * a gesture nothing announces is a gesture nobody finds.
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
    var query by remember { mutableStateOf("") }
    Column(Modifier.fillMaxSize()) {
        ScreenBar("Browse", onClose)
        LazyColumn(Modifier.weight(1f).fillMaxWidth()) {
            if (query.isBlank()) {
                rows(children, expanded, "", 0, model, onOpen)
            } else {
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
        Field(query, { query = it; model.search(it) }, "Search notes")
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
