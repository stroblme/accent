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

/**
 * The app is two things, and which one it is depends on how it was started.
 *
 * Opened from the launcher it is a vault: a tree, a switcher and a note. Opened from a `VIEW`
 * intent it is a PDF viewer with a pen and no vault at all — the same split the desktop makes
 * between a vault window and a loose one.
 */
class MainActivity : ComponentActivity() {
    private val model: VaultModel by viewModels()

    /**
     * The PDF a `VIEW` intent asked for, if the app was started by one.
     *
     * On the activity rather than inside `setContent`, because [onNewIntent] is what changes it
     * and nothing inside a composition can be reached from there. That is the whole reason the
     * activity used to be rebuilt for every intent.
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
     * Another intent, which is only worth anything if it names a different PDF.
     *
     * Coming back from the launcher is an intent that says nothing about what should be shown, and
     * rebuilding the activity for it is a reader sent back to the top of the note they were halfway
     * through — the `WebView` is built again with everything else. So the intent that decided the
     * screen is the one that stays set, which is also what a later config change reads back.
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
