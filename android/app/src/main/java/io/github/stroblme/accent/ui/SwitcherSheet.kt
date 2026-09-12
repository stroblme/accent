package io.github.stroblme.accent.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.stroblme.accent.Recents
import io.github.stroblme.accent.VaultModel
import io.github.stroblme.accent.ffi.fuzzyRank
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File

/**
 * The quick switcher, and the command palette behind the same toggle.
 *
 * Two modes rather than two sheets, because on the desktop they are two modes of one dialog and
 * the query means the same thing in both. What a phone loses is the `>` prefix that switches
 * them; a chip does it instead.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SwitcherSheet(model: VaultModel, onDismiss: () -> Unit) {
    var commands by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }
    var rows by remember { mutableStateOf<List<String>>(emptyList()) }

    val corpus = if (commands) Commands.map { it.label } else model.corpus
    val kind = if (commands) Recents.Kind.Commands else Recents.Kind.Notes

    LaunchedEffect(query, commands, corpus.size) {
        rows = withContext(Dispatchers.Default) {
            if (query.isBlank() && !commands) {
                model.recents.list(Recents.Kind.Notes).filter { it in corpus }.take(50)
            } else {
                val ranks = model.recents.ranks(kind, corpus)
                fuzzyRank(query, corpus, ranks).map { corpus[it.toInt()] }
            }
        }
    }

    ModalBottomSheet(
        onDismissRequest = onDismiss,
        containerColor = MaterialTheme.colorScheme.surface,
    ) {
        Column(Modifier.fillMaxWidth().heightIn(max = 560.dp)) {
            Row(Modifier.padding(horizontal = Gutter), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilterChip(
                    selected = !commands,
                    onClick = { commands = false; query = "" },
                    label = { Text("Notes") },
                )
                FilterChip(
                    selected = commands,
                    onClick = { commands = true; query = "" },
                    label = { Text("Commands") },
                )
            }
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                placeholder = { Text(if (commands) "Run a command" else "Go to a note") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(Gutter),
            )
            LazyColumn(Modifier.fillMaxWidth()) {
                items(rows, key = { it }) { row ->
                    ListItem(
                        headlineContent = {
                            Text(
                                if (commands) row else File(row).name,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        },
                        supportingContent = if (commands) null else ({
                            Text(row, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                        }),
                        colors = flatRow(),
                        modifier = Modifier.row {
                            if (commands) {
                                model.recents.touch(Recents.Kind.Commands, row)
                                Commands.first { it.label == row }.run(model)
                            } else {
                                model.openFile(row)
                            }
                            onDismiss()
                        },
                    )
                }
            }
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
