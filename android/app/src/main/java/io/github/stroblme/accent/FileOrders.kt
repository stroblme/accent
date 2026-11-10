package io.github.stroblme.accent

import android.content.Context
import io.github.stroblme.accent.ffi.Order
import io.github.stroblme.accent.ffi.Sort

/**
 * The order Browse's tree lists a folder in: each vault's sort, and Folders Among Files for every
 * vault, as the desktop keeps them in its session and its config. Kept on the phone.
 */
class FileOrders(context: Context) {
    private val prefs = context.getSharedPreferences("files", Context.MODE_PRIVATE)

    /** The order [root] was last listed in; file name A to Z for a vault never sorted. */
    fun of(root: String): Order =
        Order(sortNamed(prefs.getString("sort:$root", null)), prefs.getBoolean(AMONG, false))

    fun keep(root: String, order: Order) {
        prefs.edit()
            .putString("sort:$root", order.sort.name)
            .putBoolean(AMONG, order.foldersAmongFiles)
            .apply()
    }

    private companion object {
        const val AMONG = "folders-among-files"
    }
}

/**
 * The desktop's sorts less its created orders: the core reads no creation time on Android (Rust's
 * `Metadata::created` is unsupported there), so those would sort as the modified ones do.
 */
val Sorts = listOf(Sort.NAME_ASC, Sort.NAME_DESC, Sort.MODIFIED_NEW, Sort.MODIFIED_OLD)

/** The sort a stored [name] names, or file name A to Z for none or one Android does not offer. */
internal fun sortNamed(name: String?): Sort = Sorts.find { it.name == name } ?: Sort.NAME_ASC
