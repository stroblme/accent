package io.github.stroblme.accent

import android.content.Context
import android.system.Os
import android.util.Log
import java.io.File

/**
 * What has to be true before the core is called the first time.
 *
 * The core writes its config, its session state and its index cache to the XDG directories,
 * because that is where they belong on the machine it was written for. Android has no `HOME` to
 * fall back on, so it is told where those are instead — which is the whole of the porting work,
 * and the reason none of this needed a `#[cfg(target_os = "android")]` in the core.
 */
object Native {
    private const val TAG = "accent"

    /** Whether libpdfium loaded. Without it the app still reads notes; PDFs show a message. */
    var pdfium: Boolean = false
        private set

    fun setUp(context: Context) {
        val files = context.filesDir
        // The index is a disposable cache, but `cacheDir` is what Android empties under pressure
        // and re-indexing a large vault is the one slow thing the app does. `noBackupFilesDir` is
        // private, survives, and is left out of cloud backups, which a rebuildable cache should be.
        val cache = File(context.noBackupFilesDir, "cache")
        setEnv("XDG_CONFIG_HOME", File(files, "config"))
        setEnv("XDG_STATE_HOME", File(files, "state"))
        setEnv("XDG_DATA_HOME", File(files, "data"))
        setEnv("XDG_CACHE_HOME", cache)

        // Loaded here rather than on the first page, so a missing library is one clear line in
        // the log at start-up instead of a failure inside a render. The core reaches it through
        // `dlopen` afterwards, which finds what the app has already loaded.
        pdfium = runCatching { System.loadLibrary("pdfium") }
            .onFailure { Log.e(TAG, "libpdfium did not load; PDFs will not open", it) }
            .isSuccess
    }

    private fun setEnv(name: String, dir: File) {
        dir.mkdirs()
        Os.setenv(name, dir.absolutePath, true)
    }
}
