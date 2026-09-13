package io.github.stroblme.accent.ui

import android.content.Intent
import android.os.Build
import android.os.Environment
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import io.github.stroblme.accent.Recents
import io.github.stroblme.accent.VaultModel
import java.io.File

/**
 * Which folder the vault is.
 *
 * A vault is a Syncthing folder full of symlinks and tens of thousands of files, and the Storage
 * Access Framework can neither see one nor walk it at any speed (ROADMAP §7.A), so the app asks
 * for all-files access and then works in real paths. The folder picker is only used to *name* the
 * folder; everything after that is `open(2)`.
 */
@Composable
fun VaultPickerScreen(model: VaultModel) {
    val context = LocalContext.current
    var granted by remember { mutableStateOf(hasAllFiles()) }
    val recents = remember { model.recents.list(Recents.Kind.Vaults) }

    val pick = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let { model.open(pathOf(it)) }
    }

    Scaffold { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).padding(horizontal = 24.dp),
            verticalArrangement = Arrangement.Center,
        ) {
            Text("Accent", style = MaterialTheme.typography.headlineSmall)
            Spacer(Modifier.height(8.dp))
            Text(
                "Open the folder your notes are in.",
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(24.dp))

            if (!granted) {
                Text(
                    "Accent reads your notes where they already are, so it needs access to all " +
                        "files. Nothing leaves the device.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(12.dp))
                FilledTonalButton(onClick = {
                    context.startActivity(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                }) { Text("Allow access") }
                Spacer(Modifier.height(24.dp))
            }

            Button(onClick = { pick.launch(null) }, enabled = true) { Text("Choose folder") }

            if (recents.isNotEmpty()) {
                Spacer(Modifier.height(32.dp))
                Text("Recent", style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant)
                LazyColumn {
                    items(recents) { path ->
                        ListItem(
                            headlineContent = { Text(File(path).name) },
                            supportingContent = {
                                Text(path, maxLines = 1, overflow = TextOverflow.MiddleEllipsis)
                            },
                            colors = flatRow(),
                            modifier = Modifier.row { model.open(path) },
                        )
                    }
                }
            }
        }
    }
    // Answered in Settings, in another activity: this screen only learns of it on the way back.
    LifecycleResumeEffect(Unit) {
        granted = hasAllFiles()
        onPauseOrDispose {}
    }
}

private fun hasAllFiles(): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.R || Environment.isExternalStorageManager()

/**
 * The real path behind a tree URI.
 *
 * The picker answers with `content://…/tree/primary:Notes`, whose document id names the volume
 * and the path inside it. `primary` is the device's own shared storage, which is where a
 * Syncthing folder lives; anything else is a removable volume under `/storage`.
 */
private fun pathOf(uri: android.net.Uri): String {
    val id = android.provider.DocumentsContract.getTreeDocumentId(uri)
    val (volume, rest) = id.split(':', limit = 2).let { it[0] to it.getOrElse(1) { "" } }
    val root = when (volume) {
        "primary" -> Environment.getExternalStorageDirectory().absolutePath
        else -> "/storage/$volume"
    }
    return if (rest.isEmpty()) root else "$root/$rest"
}
