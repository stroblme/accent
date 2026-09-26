package io.github.stroblme.accent

import android.content.Context
import android.system.Os
import android.util.Log
import io.github.stroblme.accent.ffi.uniffiEnsureInitialized
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
        runCatching { System.loadLibrary("pdfium") }
            .onFailure { Log.e(TAG, "libpdfium did not load; PDFs will not open", it) }

        // The core, bound now rather than on the first call, and checked against the bindings
        // it was built with: a library that does not match them throws here, once, at launch,
        // where it would otherwise fail on whichever call reached a changed function first.
        // Not caught, because without the core there is no app to run.
        uniffiEnsureInitialized()
    }

    private fun setEnv(name: String, dir: File) {
        dir.mkdirs()
        Os.setenv(name, dir.absolutePath, true)
    }
}
