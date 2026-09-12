package io.github.stroblme.accent

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.collectAsState
import io.github.stroblme.accent.ui.AccentTheme
import io.github.stroblme.accent.ui.HomeScreen
import io.github.stroblme.accent.ui.LoosePdfScreen
import io.github.stroblme.accent.ui.VaultPickerScreen

/**
 * The app is two things, and which one it is depends on how it was started.
 *
 * Opened from the launcher it is a vault: a tree, a switcher and a note. Opened from a `VIEW`
 * intent it is a PDF viewer with a pen and no vault at all — the same split the desktop makes
 * between a vault window and a loose one.
 */
class MainActivity : ComponentActivity() {
    private val model: VaultModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            var loose by remember { mutableStateOf(intent.pdfUri()) }
            AccentTheme {
                val state by model.state.collectAsState()
                val pdf = loose
                when {
                    pdf != null -> LoosePdfScreen(uri = pdf, onClose = { finish() })
                    state.root == null -> VaultPickerScreen(model)
                    else -> HomeScreen(model)
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        recreate()
    }

    override fun onResume() {
        super.onResume()
        // Nothing reaches an unwatched vault on its own, so this is where it catches up with
        // whatever Syncthing did while the app was in the background.
        if (model.state.value.root != null) model.rescan()
    }
}

private fun Intent.pdfUri(): Uri? =
    if (action == Intent.ACTION_VIEW) data else null
