package io.github.stroblme.accent

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import io.github.stroblme.accent.ffi.AccentException
import io.github.stroblme.accent.ffi.Etag
import io.github.stroblme.accent.ffi.Event
import io.github.stroblme.accent.ffi.FileRow
import io.github.stroblme.accent.ffi.Phase
import io.github.stroblme.accent.ffi.Progress
import io.github.stroblme.accent.ffi.SearchHit
import io.github.stroblme.accent.ffi.Vault
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** What the note in front of the reader is, and what the vault has to say about it. */
data class Open(
    val rel: String,
    val text: String = "",
    val etag: Etag? = null,
    /** Somebody else wrote the file while it was open and dirty: autosave has stopped. */
    val changedOnDisk: Boolean = false,
    /** Conflict copies Syncthing left beside it. */
    val conflicts: List<String> = emptyList(),
)

data class VaultState(
    val root: String? = null,
    val indexing: Boolean = false,
    /** Files the walk has been through so far, for as long as one is running. */
    val scanned: Long = 0,
    /**
     * Which half of the work is running. Nothing is in the index during [Phase.SCAN] — the walk
     * is still finding the files — so that is the one phase where the tree really has nothing to
     * show and the app has to say so rather than invite a reader in.
     */
    val phase: Phase? = null,
    /** The directories whose children have been listed, so the tree redraws in place. */
    val children: Map<String, List<FileRow>> = emptyMap(),
    val expanded: Set<String> = setOf(""),
    val results: List<SearchHit> = emptyList(),
    val open: Open? = null,
    val pdf: String? = null,
    val message: String? = null,
)

/**
 * The vault, its events, and the one note in front of the reader.
 *
 * Every call into the core is blocking and goes on [Dispatchers.IO]; nothing here touches the
 * main thread except the state it publishes. Nothing arrives from the filesystem on its own —
 * the vault is opened unwatched — so [rescan] is what the app calls when it comes back.
 */
class VaultModel(app: Application) : AndroidViewModel(app) {
    private val _state = MutableStateFlow(VaultState())
    val state: StateFlow<VaultState> = _state.asStateFlow()

    val recents = Recents(app)

    private var vault: Vault? = null

    /** Every file in the vault, for the switcher. See [corpus]. */
    private var corpus: List<String> = emptyList()

    /** How far the batch being applied said the walk had got, and when the tree last caught up. */
    private var seen: Progress? = null
    private var lastRelist = 0L

    fun open(root: String) {
        viewModelScope.launch {
            val opened = withContext(Dispatchers.IO) { runCatching { Vault.open(root) } }
            opened
                .onSuccess { v ->
                    vault?.close()
                    vault = v
                    recents.touch(Recents.Kind.Vaults, root)
                    _state.update { VaultState(root = v.root(), indexing = true) }
                    listen()
                    refresh()
                }
                .onFailure { fail("Cannot open this folder", it) }
        }
    }

    /** Drain the vault's events for as long as the vault is open. */
    private fun listen() = viewModelScope.launch(Dispatchers.IO) {
        val v = vault ?: return@launch
        while (isActive && vault === v) {
            // Half a second is short enough that closing the vault is not felt and long enough
            // that a quiet app is not waking up for nothing.
            val batch = runCatching { v.nextEvents(500u) }.getOrElse { return@launch }
            if (batch.isNotEmpty()) apply(batch)
        }
    }

    private suspend fun apply(batch: List<Event>) {
        var reindexed = false
        val touched = mutableSetOf<String>()
        for (event in batch) when (event) {
            is Event.Reconciled -> reindexed = true
            is Event.DirsChanged -> touched += event.dirs
            is Event.FileChanged -> onChanged(event.rel)
            is Event.FileRemoved -> touched += parentOf(event.rel)
            is Event.FileRenamed -> {
                touched += parentOf(event.from)
                touched += parentOf(event.to)
            }
            is Event.Conflict -> onConflict(event.original)
            is Event.Error -> _state.update { it.copy(message = event.message) }
            is Event.Progress -> seen = event.progress
        }
        if (reindexed) {
            _state.update { it.copy(indexing = false, scanned = 0, phase = null) }
            corpus = emptyList()
            relist(_state.value.expanded)
        } else if (touched.isNotEmpty()) {
            relist(touched.intersect(_state.value.children.keys))
        } else {
            seen?.let { walking(it) }
        }
        seen = null
    }

    /**
     * Say how far the walk has got, and let the tree catch up with it.
     *
     * The index fills as the walk goes, in batches, so the directories already in it can be
     * listed while the rest are still being found. On a vault of any size that is the difference
     * between a tree that appears when the walk ends and one that is usable from the start.
     */
    private suspend fun walking(p: Progress) {
        _state.update { it.copy(indexing = true, scanned = p.done.toLong(), phase = p.phase) }
        // Nothing has been written yet while the walk is still finding files.
        if (p.phase == Phase.SCAN) return
        val now = System.currentTimeMillis()
        if (now - lastRelist < RELIST_EVERY_MS) return
        lastRelist = now
        relist(_state.value.expanded)
    }

    /** The switcher's corpus: every file in the vault, fetched the first time it is asked for. */
    suspend fun corpus(): List<String> {
        corpus.takeIf { it.isNotEmpty() }?.let { return it }
        val v = vault ?: return emptyList()
        // Tens of thousands of strings in one call: worth doing when the switcher opens, which is
        // the only thing that wants them, rather than after every reconcile.
        corpus = withContext(Dispatchers.IO) {
            runCatching { v.filePaths(false) }.getOrDefault(emptyList())
        }
        return corpus
    }

    /** List every directory the tree has open again. */
    fun refresh() = viewModelScope.launch { relist(_state.value.expanded) }

    private suspend fun relist(dirs: Set<String>) = withContext(Dispatchers.IO) {
        val v = vault ?: return@withContext
        val listed = dirs.mapNotNull { dir -> runCatching { dir to v.listDir(dir) }.getOrNull() }
        _state.update { it.copy(children = it.children + listed) }
    }

    fun toggle(dir: String) {
        val open = _state.value.expanded
        if (dir in open) {
            _state.update { it.copy(expanded = open - dir) }
        } else {
            _state.update { it.copy(expanded = open + dir) }
            viewModelScope.launch { relist(setOf(dir)) }
        }
    }

    /** Walk the files again: what the app does instead of watching them. */
    fun rescan() = viewModelScope.launch(Dispatchers.IO) {
        val v = vault ?: return@launch
        _state.update { it.copy(indexing = true) }
        runCatching { v.rescan() }
        // An open note may have been changed by Syncthing while the app was away.
        _state.value.open?.let { onChanged(it.rel) }
    }

    // ------------------------------------------------------------------------------ one file

    fun openFile(rel: String) {
        val v = vault ?: return
        recents.touch(Recents.Kind.Notes, rel)
        if (rel.endsWith(".pdf", ignoreCase = true)) {
            viewModelScope.launch {
                val path = withContext(Dispatchers.IO) { runCatching { v.pathOf(rel) } }
                path.onSuccess { p -> _state.update { it.copy(pdf = p, open = null) } }
                    .onFailure { fail("Cannot open this file", it) }
            }
            return
        }
        viewModelScope.launch {
            val read = withContext(Dispatchers.IO) {
                runCatching { v.read(rel) to v.conflictsOf(rel) }
            }
            read.onSuccess { (note, conflicts) ->
                _state.update {
                    it.copy(pdf = null, open = Open(rel, note.text, note.etag, conflicts = conflicts))
                }
            }.onFailure { fail("Cannot read this note", it) }
        }
    }

    fun close() = _state.update { it.copy(open = null, pdf = null) }

    /**
     * Follow a link out of the rendered note.
     *
     * The target is what the link spelled — a note's name, a path, possibly with a `#heading` or
     * a PDF's `page=` after it. The index is what knows which file that is; a target it cannot
     * place is a link to a note nobody has written yet.
     */
    fun openLink(target: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        val name = target.substringBefore('#')
        val found = withContext(Dispatchers.IO) { runCatching { v.resolveLink(name) }.getOrNull() }
        when (val rel = found) {
            null -> _state.update { it.copy(message = "No note called \"$name\"") }
            else -> openFile(rel)
        }
    }

    /**
     * Write the note back, refusing rather than resolving when the file has moved under it.
     *
     * A refusal leaves the buffer alone and raises the banner: that buffer holds the only copy of
     * both the edits and the answer nobody has given yet. Same rule as the desktop.
     */
    fun save(text: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        val open = _state.value.open ?: return@launch
        if (open.changedOnDisk) return@launch
        val written = withContext(Dispatchers.IO) {
            runCatching { v.save(open.rel, text, open.etag) }
        }
        written.onSuccess { etag ->
            _state.update {
                if (it.open?.rel != open.rel) it else it.copy(open = it.open.copy(text = text, etag = etag))
            }
        }.onFailure { e ->
            when (e) {
                is AccentException.ChangedOnDisk ->
                    _state.update { it.copy(open = it.open?.copy(changedOnDisk = true)) }
                else -> fail("Cannot save this note", e)
            }
        }
    }

    /** Take what is on disk, dropping the edits in the buffer. */
    fun reload() {
        _state.update { it.copy(open = it.open?.copy(changedOnDisk = false)) }
        _state.value.open?.let { openFile(it.rel) }
    }

    fun newNote(rel: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        val made = withContext(Dispatchers.IO) { runCatching { v.createNote(rel, null) } }
        made.onSuccess { openFile(rel) }.onFailure { fail("Cannot create this note", it) }
    }

    // ----------------------------------------------------------------------- sync conflicts

    /** Keep the conflict copy: its text becomes the note's and the copy goes. */
    fun keepTheirs(conflict: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        val open = _state.value.open ?: return@launch
        val done = withContext(Dispatchers.IO) { runCatching { v.adoptConflict(open.rel, conflict) } }
        done.onSuccess { openFile(open.rel) }.onFailure { fail("Cannot take the other copy", it) }
    }

    /** Keep this note as it stands and delete the conflict copy. */
    fun keepMine(conflict: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        val done = withContext(Dispatchers.IO) { runCatching { v.delete(conflict) } }
        done.onSuccess {
            _state.update {
                it.copy(open = it.open?.copy(conflicts = it.open.conflicts - conflict))
            }
        }.onFailure { fail("Cannot remove the other copy", it) }
    }

    private fun onConflict(original: String) = _state.update { s ->
        if (s.open?.rel != original) s else s.copy(open = s.open.copy(conflicts = emptyList()))
    }.also { _state.value.open?.let { open -> refreshConflicts(open.rel) } }

    private fun refreshConflicts(rel: String) = viewModelScope.launch(Dispatchers.IO) {
        val v = vault ?: return@launch
        val found = runCatching { v.conflictsOf(rel) }.getOrDefault(emptyList())
        _state.update { if (it.open?.rel != rel) it else it.copy(open = it.open.copy(conflicts = found)) }
    }

    // ------------------------------------------------------------------------------- search

    fun search(query: String) = viewModelScope.launch(Dispatchers.IO) {
        val v = vault ?: return@launch
        if (query.isBlank()) {
            _state.update { it.copy(results = emptyList()) }
            return@launch
        }
        val hits = runCatching { v.search(query, 100u, false) }.getOrDefault(emptyList())
        _state.update { it.copy(results = hits) }
    }

    fun said(message: String?) = _state.update { it.copy(message = message) }

    // ------------------------------------------------------------------------------ helpers

    /** Someone else wrote a file. Only the open note needs to know. */
    private fun onChanged(rel: String) {
        val open = _state.value.open ?: return
        if (open.rel != rel) return
        refreshConflicts(rel)
        viewModelScope.launch {
            val v = vault ?: return@launch
            val read = withContext(Dispatchers.IO) { runCatching { v.read(rel) } }
            val note = read.getOrNull() ?: return@launch
            _state.update { s ->
                val cur = s.open ?: return@update s
                when {
                    // Nothing was typed here, so taking the new text loses nothing.
                    cur.etag != null && cur.text == note.text -> s.copy(open = cur.copy(etag = note.etag))
                    else -> s.copy(open = cur.copy(changedOnDisk = true))
                }
            }
        }
    }

    private fun fail(what: String, e: Throwable) {
        val why = (e as? AccentException.Failed)?.reason ?: e.message
        _state.update { it.copy(message = if (why.isNullOrBlank()) what else "$what: $why") }
    }

    override fun onCleared() {
        vault?.close()
        vault = null
    }

    companion object {
        /** How often the tree catches up with a running walk. Often enough to watch it fill. */
        private const val RELIST_EVERY_MS = 700L

        fun parentOf(rel: String): String = rel.substringBeforeLast('/', "")
    }
}
