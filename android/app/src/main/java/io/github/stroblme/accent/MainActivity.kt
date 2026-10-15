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
import androidx.compose.runtime.setValue
import androidx.compose.runtime.collectAsState
import io.github.stroblme.accent.ui.AccentTheme
import io.github.stroblme.accent.ui.HomeScreen
import io.github.stroblme.accent.ui.LoosePdfScreen
import io.github.stroblme.accent.ui.VaultPickerScreen

/** Launcher starts a vault editor; a `VIEW` intent starts a standalone PDF viewer. */
class MainActivity : ComponentActivity() {
    private val model: VaultModel by viewModels()

    /**
     * PDF selected by a `VIEW` intent. Activity state lets [onNewIntent] update it outside composition.
     */
    private var loose by mutableStateOf<Uri?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        loose = intent.pdfUri()
        setContent {
            AccentTheme {
                val state by model.state.collectAsState()
                val pdf = loose
                when {
                    pdf != null -> LoosePdfScreen(uri = pdf)
                    state.root == null -> VaultPickerScreen(model)
                    else -> HomeScreen(model)
                }
            }
        }
    }

    /**
     * Apply a new `VIEW` intent only for a different PDF; unrelated intents retain the current screen.
     */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        val pdf = intent.pdfUri() ?: return
        setIntent(intent)
        loose = pdf
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
