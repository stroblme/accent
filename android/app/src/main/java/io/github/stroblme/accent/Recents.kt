package io.github.stroblme.accent

import android.content.Context

/** Persist recent vaults, notes, and commands separately from desktop window session state. */
class Recents(context: Context) {
    private val prefs = context.getSharedPreferences("recents", Context.MODE_PRIVATE)

    fun list(of: Kind): List<String> =
        prefs.getString(of.key, "").orEmpty().split('\n').filter { it.isNotEmpty() }

    /** Move [value] to the front, keeping the list to [Kind.cap]. */
    fun touch(of: Kind, value: String) {
        val kept = (listOf(value) + list(of).filter { it != value }).take(of.cap)
        prefs.edit().putString(of.key, kept.joinToString("\n")).apply()
    }

    /** Return recency ranks for [haystacks] used by the switcher. */
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
