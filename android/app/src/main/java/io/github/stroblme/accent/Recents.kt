package io.github.stroblme.accent

import android.content.Context

/**
 * The few things the app remembers between runs: which vaults have been opened, which notes and
 * commands were reached for last.
 *
 * Deliberately not the core's `Config` and `Session`. Those hold a window's panes, its split
 * ratios and its zoom, none of which exist here; and a phone's idea of "where I was" is its own
 * question. What the two do share is the vault, and that is a path.
 */
class Recents(context: Context) {
    private val prefs = context.getSharedPreferences("recents", Context.MODE_PRIVATE)

    fun list(of: Kind): List<String> =
        prefs.getString(of.key, "").orEmpty().split('\n').filter { it.isNotEmpty() }

    /** Move [value] to the front, keeping the list to [Kind.cap]. */
    fun touch(of: Kind, value: String) {
        val kept = (listOf(value) + list(of).filter { it != value }).take(of.cap)
        prefs.edit().putString(of.key, kept.joinToString("\n")).apply()
    }

    fun forget(of: Kind, value: String) {
        prefs.edit().putString(of.key, list(of).filter { it != value }.joinToString("\n")).apply()
    }

    /** How recently each of [haystacks] was used, for the switcher's ranking. */
    fun ranks(of: Kind, haystacks: List<String>): List<UInt?> {
        val order = list(of).withIndex().associate { (i, v) -> v to i.toUInt() }
        return haystacks.map { order[it] }
    }

    enum class Kind(val key: String, val cap: Int) {
        Vaults("vaults", 8),
        Notes("notes", 50),
        Commands("commands", 20),
    }
}
