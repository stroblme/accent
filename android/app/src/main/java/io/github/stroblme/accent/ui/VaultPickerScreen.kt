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
 * Use the system picker to name a vault folder, then open its real path.
 * All-files access is required for symlinked Syncthing vaults (MOBILE_DESIGN.md, Architecture).
 */
@Composable
fun VaultPickerScreen(model: VaultModel) {
    val context = LocalContext.current
    var granted by remember { mutableStateOf(hasAllFiles()) }
    val recents = remember { model.recents.list(Recents.Kind.Vaults) }

    val pick = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let { model.open(pathOf(it)) }
    }

    // A folder that could not be opened comes back here, and this is where it says why.
    val snackbar = remember { SnackbarHostState() }
    val message = model.state.collectAsState().value.message
    message?.let {
        LaunchedEffect(it) {
            snackbar.showSnackbar(it)
            model.said(null)
        }
    }

    Scaffold(snackbarHost = { SnackbarHost(snackbar) }) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).padding(horizontal = 24.dp),
            verticalArrangement = Arrangement.Center,
        ) {
            Text("Accent", style = MaterialTheme.typography.headlineSmall)
            Spacer(Modifier.height(8.dp))
            Text(
                "Choose the folder that holds your notes.",
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(24.dp))

            if (!granted) {
                Text(
                    "To read notes in place, Accent needs access to all files. " +
                        "It doesn't upload your notes.",
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
                Text("Recent vaults", style = MaterialTheme.typography.labelMedium,
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

/** Resolve a tree URI document ID to a shared-storage or removable-volume path. */
private fun pathOf(uri: android.net.Uri): String {
    val id = android.provider.DocumentsContract.getTreeDocumentId(uri)
    val (volume, rest) = id.split(':', limit = 2).let { it[0] to it.getOrElse(1) { "" } }
    val root = when (volume) {
        "primary" -> Environment.getExternalStorageDirectory().absolutePath
        else -> "/storage/$volume"
    }
    return if (rest.isEmpty()) root else "$root/$rest"
}
