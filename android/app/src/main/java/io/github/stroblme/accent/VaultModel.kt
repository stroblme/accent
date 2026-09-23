package io.github.stroblme.accent

import android.app.Application
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.runtime.snapshotFlow
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
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Put a note's text in the buffer as its own, with nothing to undo back into.
 *
 * The state object outlives every note it holds, and a programmatic edit is an undo step like any
 * other, so the history has to go with the text: an undo that walked back past the load would put
 * one file's words in another's.
 */
fun TextFieldState.load(text: String) {
    setTextAndPlaceCursorAtEnd(text)
    undoState.clearHistory()
}

/** What the note in front of the reader is, and what the vault has to say about it. */
data class Open(
    val rel: String,
    /**
     * The text as the vault has it: what was read off disk, or what was last written back.
     *
     * What the *reader* has is [VaultModel.buffer], and the difference between the two is the
     * whole of "this note is dirty" — it is what autosave compares against and what a write
     * replaces.
     */
    val text: String = "",
    val etag: Etag? = null,
    /** Somebody else wrote the file while it was open and dirty: autosave has stopped. */
    val changedOnDisk: Boolean = false,
    /** Conflict copies Syncthing left beside it. */
    val conflicts: List<String> = emptyList(),
    /**
     * The query that led here, until the page has been marked with it.
     *
     * A search hit and a file row are the same act of opening a note, and this is the only thing
     * that tells them apart: one was looked for, the other was picked off a list. Consumed once
     * the way [VaultState.message] is ([found]), because where a reader was sent is a moment
     * rather than something the note now has.
     */
    val find: String? = null,
    /**
     * Whether the reader has the note's own find open ([VaultModel.finding]).
     *
     * Not [find]: that is one word the app was handed and marks once, this is a bar the reader
     * types into. It belongs to the note in front, so opening another puts it away.
     */
    val finding: Boolean = false,
    /**
     * The reader is on the way out of the note — closing it, closing the vault, opening another —
     * over edits saving was paused on, and has been asked which version to keep
     * ([VaultModel.answer]).
     */
    val leaving: Boolean = false,
) {
    /** Whether [typed] holds anything the vault does not have: what a write would put there. */
    fun dirty(typed: CharSequence): Boolean = typed.toString() != text

    /**
     * Whether taking [typed] away would lose edits: saving is paused while the file has moved
     * under the note, so no exit can write them. A banner over a note nobody typed into has
     * nothing to lose.
     */
    fun wouldLose(typed: CharSequence): Boolean = changedOnDisk && dirty(typed)
}

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
    /**
     * Whether the index has anything in it yet: what the start screen's status line says in
     * words, and the one thing the Browse button is allowed to depend on.
     *
     * A latch, because [indexing] is about work running and not about the index being empty. It
     * is false for exactly one stretch of a vault's life — its first walk, until the first batch
     * of files has been read — and a walk over an index that already has files (the rescan every
     * resume starts) leaves it alone, those files still being there to browse.
     */
    val ready: Boolean = false,
    /** The directories whose children have been listed, so the tree redraws in place. */
    val children: Map<String, List<FileRow>> = emptyMap(),
    val expanded: Set<String> = setOf(""),
    val results: List<SearchHit> = emptyList(),
    val open: Open? = null,
    val pdf: String? = null,
    val message: String? = null,
)

/**
 * What the switcher ranks: every file, then every note a link names that is not there yet, by the
 * path creating it would give it.
 *
 * One list, so the two rank together, and the files first: the ranking is stable, so a file that
 * is there leads a note only linked to at the same score.
 */
class Corpus(files: List<String>, missing: List<String>) {
    val paths = files + missing

    /** The notes in [paths] that are not there yet: a pick writes one rather than opens it. */
    val unwritten = missing.toHashSet()
}

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

    /**
     * The note's text, as the reader has it: the editor's field state, hoisted.
     *
     * The editor is not the only thing that touches it. Taking the version on disk replaces it,
     * closing the vault has to write it out before the handle goes, and the rendered view draws
     * from it so that an edit cannot be in one view and not the other — none of which a state
     * remembered inside the editor could be reached for. One object for the app's life, because
     * building another throws away the undo history with it; the text in it is replaced ([load]).
     */
    val buffer = TextFieldState()

    private var vault: Vault? = null

    /** The exit [leave] is holding while the reader answers, if there is one. */
    private var held: (suspend () -> Unit)? = null

    /** The switcher's files. See [corpus]. */
    private var corpus: Corpus? = null

    /** How far the batch being applied said the walk had got, and when the tree last caught up. */
    private var seen: Progress? = null
    private var lastRelist = 0L

    init {
        // Autosave: a pause in the typing, not a queue of them — `collectLatest` drops the wait
        // the moment the next keystroke lands. It runs for as long as the model does rather than
        // for as long as the editor is composed, so leaving the editor inside that second no
        // longer needs a write of its own, and what it is compared against is read when the wait
        // is over: typing a word and taking it back again saves nothing, and a note replaced
        // under it (a reload, another note) is left alone.
        viewModelScope.launch {
            snapshotFlow { buffer.text.toString() }.collectLatest { text ->
                delay(SAVE_AFTER_MS)
                if (text != _state.value.open?.text) write(text)
            }
        }
    }

    fun open(root: String) {
        viewModelScope.launch {
            val opened = withContext(Dispatchers.IO) { runCatching { Vault.open(root) } }
            opened
                .onSuccess { v ->
                    release(vault)
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
            // The walk is over, so whatever the vault holds is in the index.
            _state.update { it.copy(indexing = false, scanned = 0, phase = null, ready = true) }
            corpus = null
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
        // Out of [Phase.SCAN] with files behind it is the moment there is something to open, and
        // it is the moment the status line says so.
        val read = p.phase != Phase.SCAN && p.done > 0uL
        _state.update {
            it.copy(
                indexing = true,
                scanned = p.done.toLong(),
                phase = p.phase,
                ready = it.ready || read,
            )
        }
        // Nothing has been written yet while the walk is still finding files.
        if (p.phase == Phase.SCAN) return
        val now = System.currentTimeMillis()
        if (now - lastRelist < RELIST_EVERY_MS) return
        lastRelist = now
        relist(_state.value.expanded)
    }

    /** The switcher's corpus, fetched the first time it is asked for. */
    suspend fun corpus(): Corpus {
        corpus?.takeIf { it.paths.isNotEmpty() }?.let { return it }
        val v = vault ?: return Corpus(emptyList(), emptyList())
        // Tens of thousands of strings in one call: worth doing when the switcher opens, which is
        // the only thing that wants them, rather than after every reconcile.
        val read = withContext(Dispatchers.IO) {
            Corpus(
                runCatching { v.filePaths(false) }.getOrDefault(emptyList()),
                runCatching { v.missingNotes() }.getOrDefault(emptyList()),
            )
        }
        corpus = read
        return read
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

    /**
     * Open a file, and — for a search hit — say what was being looked for.
     *
     * [find] is the query rather than [SearchHit.at]: the hit's range is bytes into the markdown
     * source and the screen holds the page that source rendered to, so the only thing that
     * survives the crossing is the words. A PDF drops it; there is no find in that reader yet.
     */
    fun openFile(rel: String, find: String? = null) {
        val v = vault ?: return
        // What is in the buffer belongs to the note it was typed into, and the buffer is about to
        // hold another note's text: a write left pending across the swap would put these words in
        // that file.
        leave {
            recents.touch(Recents.Kind.Notes, rel)
            if (rel.endsWith(".pdf", ignoreCase = true)) {
                val path = withContext(Dispatchers.IO) { runCatching { v.pathOf(rel) } }
                path.onSuccess { p -> _state.update { it.copy(pdf = p, open = null) } }
                    .onFailure { fail("Cannot open this file", it) }
                return@leave
            }
            val read = withContext(Dispatchers.IO) {
                runCatching { v.read(rel) to v.conflictsOf(rel) }
            }
            read.onSuccess { (note, conflicts) ->
                // The buffer and the state in one step, so nothing composes a note's name over
                // another note's text.
                buffer.load(note.text)
                _state.update {
                    it.copy(
                        pdf = null,
                        open = Open(rel, note.text, note.etag, conflicts = conflicts, find = find),
                    )
                }
            }.onFailure { fail("Cannot read this note", it) }
        }
    }

    /** Put the note down, writing anything the pause was still holding. */
    fun close() = leave { _state.update { it.copy(open = null, pdf = null) } }

    /**
     * Put the vault down and go back to the picker.
     *
     * The recent list keeps it, so the picker offers it straight back; what goes is the handle and
     * everything the screen was reading out of it. Closed in the order [open] closes the one it
     * replaces, and for the same reason: [listen] is waiting on a half-second drain, sees that
     * `vault` is no longer the vault it was given, and leaves. Its blocked call is safe — uniffi
     * counts the calls in flight and frees the object behind the last of them, and the next one
     * throws, which that loop already treats as the end.
     */
    fun closeVault() = leave {
        // After the buffer is written rather than before, because the write goes through the
        // handle: the buffer is the only copy of whatever was typed in the last second.
        release(vault)
        vault = null
        corpus = null
        _state.value = VaultState()
    }

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
     * both the edits and the answer nobody has given yet. Same rule as the desktop. [force] is
     * that answer given as Keep mine, and writes over whatever is on disk: a save that expects no
     * particular version is the core's forced write, the desktop's Overwrite.
     */
    private suspend fun write(text: String, force: Boolean = false) {
        val v = vault ?: return
        val open = _state.value.open ?: return
        if (open.changedOnDisk && !force) return
        val written = withContext(Dispatchers.IO) {
            runCatching { v.save(open.rel, text, if (force) null else open.etag) }
        }
        written.onSuccess { etag ->
            // What was just written is what is on disk, so there is nothing left to choose between.
            _state.update {
                if (it.open?.rel != open.rel) {
                    it
                } else {
                    it.copy(open = it.open.copy(text = text, etag = etag, changedOnDisk = false))
                }
            }
        }.onFailure { e ->
            when (e) {
                is AccentException.ChangedOnDisk ->
                    _state.update { it.copy(open = it.open?.copy(changedOnDisk = true)) }
                else -> fail("Cannot save this note", e)
            }
        }
    }

    /**
     * Write what is in the buffer before whatever is about to take it away.
     *
     * The pause is a coroutine that will be cancelled with the vault or superseded by the next
     * note, so the exits ask for the write themselves. A note with saving paused is left alone:
     * that pause is the point of the banner, and writing anyway would overwrite the file the
     * reader has not chosen yet.
     */
    private suspend fun flush() {
        val text = buffer.text.toString()
        if (text != _state.value.open?.text) write(text)
    }

    /**
     * Take the buffer away — for another note, for none, or with the vault — once [flush] has
     * written it.
     *
     * Edits saving was paused on are what [flush] cannot write, and going anyway would drop them
     * with nothing said. So the exit is held instead and the reader asked which version to keep
     * ([Open.leaving]); [answer] lets it go or drops it.
     */
    private fun leave(then: suspend () -> Unit) = viewModelScope.launch {
        if (_state.value.open?.wouldLose(buffer.text) == true) {
            held = then
            _state.update { it.copy(open = it.open?.copy(leaving = true)) }
            return@launch
        }
        flush()
        then()
    }

    /**
     * The reader's answer to the exit [leave] is holding: `true` keeps their edits over the
     * version on disk and goes, `false` goes without them — saving is still paused, so nothing
     * writes them — and `null` stays, edits and banner as they were.
     */
    fun answer(keep: Boolean?) = viewModelScope.launch {
        val then = held ?: return@launch
        held = null
        _state.update { it.copy(open = it.open?.copy(leaving = false)) }
        if (keep == null) return@launch
        if (keep) {
            overwrite().join()
            // A write that failed has said so, and the edits are still only in the buffer: stay.
            if (_state.value.open?.wouldLose(buffer.text) == true) return@launch
        }
        then()
    }

    /** Keep mine: write the buffer over the version on disk. */
    fun overwrite() = viewModelScope.launch { write(buffer.text.toString(), force = true) }

    /** Take what is on disk, dropping the edits in the buffer. */
    fun reload() {
        // The banner goes first, so the read that follows is allowed to replace the buffer.
        _state.update { it.copy(open = it.open?.copy(changedOnDisk = false)) }
        _state.value.open?.let { openFile(it.rel) }
    }

    fun newNote(rel: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        val made = withContext(Dispatchers.IO) { runCatching { v.createNote(rel, null) } }
        made.onSuccess {
            // The switcher reads its files again next time, so the note is one of them there.
            corpus = null
            openFile(rel)
        }.onFailure { fail("Cannot create this note", it) }
    }

    /**
     * Write a note a link names, and open it: a "Not created" row in the switcher.
     *
     * The rows are kept from one walk to the next, so the note may have been written since they
     * were read — by Syncthing, or by this row before the index caught up — and is then opened.
     */
    fun create(rel: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        if (withContext(Dispatchers.IO) { v.exists(rel) }) openFile(rel) else newNote(rel)
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

    /** The query has been marked on the page. One-shot, the same way [said] is. */
    fun found() = _state.update { it.copy(open = it.open?.copy(find = null)) }

    /**
     * Open or put away the note's own find.
     *
     * With nothing in front there is nothing to find in and this does nothing, which is what
     * [close] already does from the same palette.
     */
    fun finding(on: Boolean) = _state.update { it.copy(open = it.open?.copy(finding = on)) }

    fun said(message: String?) = _state.update { it.copy(message = message) }

    // ------------------------------------------------------------------------------ helpers

    /**
     * Someone else wrote a file. Only the open note needs to know.
     *
     * A note holding nothing the vault does not have takes the new text as it stands, as the
     * desktop reloads a clean tab; one holding edits raises the banner, since which of the two to
     * keep is the reader's call.
     */
    private fun onChanged(rel: String) {
        val open = _state.value.open ?: return
        if (open.rel != rel) return
        refreshConflicts(rel)
        viewModelScope.launch {
            val v = vault ?: return@launch
            val read = withContext(Dispatchers.IO) { runCatching { v.read(rel) } }
            val note = read.getOrNull() ?: return@launch
            val cur = _state.value.open?.takeIf { it.rel == rel } ?: return@launch
            // Nothing was typed here, so taking the new text loses nothing. The buffer and the
            // state in one step, as a note is opened.
            val take = cur.text != note.text && !cur.dirty(buffer.text)
            if (take) buffer.load(note.text)
            _state.update { s ->
                val now = s.open?.takeIf { it.rel == rel } ?: return@update s
                s.copy(
                    open = when {
                        // Only touched: the text is the one already here.
                        now.etag != null && now.text == note.text -> now.copy(etag = note.etag)
                        take -> now.copy(text = note.text, etag = note.etag, changedOnDisk = false)
                        else -> now.copy(changedOnDisk = true)
                    },
                )
            }
        }
    }

    /**
     * Put a vault handle down without holding the frame.
     *
     * The last call to go frees the object, and the core's `Drop` shuts the language sessions
     * down and joins the index worker — seconds, on a vault being reconciled for the first time.
     * On a thread of its own, so the screen that asked is not the thread that waits.
     */
    private fun release(handle: Vault?) {
        val going = handle ?: return
        Thread({ going.close() }, "vault-close").start()
    }

    private fun fail(what: String, e: Throwable) {
        val why = (e as? AccentException.Failed)?.reason ?: e.message
        _state.update { it.copy(message = if (why.isNullOrBlank()) what else "$what: $why") }
    }

    override fun onCleared() {
        release(vault)
        vault = null
    }

    companion object {
        /** How long a pause in the typing is worth a write. The same second the desktop waits. */
        private const val SAVE_AFTER_MS = 1000L

        /** How often the tree catches up with a running walk. Often enough to watch it fill. */
        private const val RELIST_EVERY_MS = 700L

        fun parentOf(rel: String): String = rel.substringBeforeLast('/', "")
    }
}
