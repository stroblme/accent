package io.github.stroblme.accent

import android.content.Context

/** Persist recent vaults, notes, and commands separately from desktop window session state. */
class Recents(context: Context) {
    private val prefs = context.getSharedPreferences("recents", Context.MODE_PRIVATE)

    fun list(of: Kind): List<String> =
        prefs.getString(of.key, "").orEmpty().split('\n').filter { it.isNotEmpty() }

    /** Move [value] to the front, keeping the list to [Kind.cap]. */
    fun touch(of: Kind, value: String) = touch(of, listOf(value))

    /** Move [values] to the front in one write, the last of them first ([fronted]). */
    fun touch(of: Kind, values: List<String>) {
        prefs.edit().putString(of.key, fronted(list(of), values, of.cap).joinToString("\n")).apply()
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

/** [list] with each of [values] moved to its front in turn, so the last is first, kept to [cap]. */
internal fun fronted(list: List<String>, values: List<String>, cap: Int): List<String> =
    (values.asReversed() + list).distinct().take(cap)
