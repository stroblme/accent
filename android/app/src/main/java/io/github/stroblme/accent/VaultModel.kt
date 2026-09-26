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
import io.github.stroblme.accent.ffi.NoteAlias
import io.github.stroblme.accent.ffi.PdfLink
import io.github.stroblme.accent.ffi.Phase
import io.github.stroblme.accent.ffi.Progress
import io.github.stroblme.accent.ffi.SearchHit
import io.github.stroblme.accent.ffi.Vault
import io.github.stroblme.accent.ffi.pdfAnchor
import io.github.stroblme.accent.ui.imageKind
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

/**
 * The PDF in front of the reader: where it is in the vault, where on disk, and where it opens —
 * and whether its find is open ([VaultModel.finding]), as a note's is ([Open.finding]).
 */
data class OpenPdf(
    val rel: String,
    val path: String,
    val at: PdfPlace? = null,
    val finding: Boolean = false,
)

/**
 * The image in front of the reader: where it is in the vault, and where on disk. It opens on the
 * image screen rather than as a note, which is what every file that was not a PDF used to do.
 */
data class OpenImage(val rel: String, val path: String)

/**
 * Where a PDF opens: [top] points down page [page], at [zoom] and pushed [panX] pixels sideways —
 * the place a reader left it, which Back from a note returns them to. Or the page a link into it
 * names and the four numbers of the passage it quotes there, shown as the selection.
 */
data class PdfPlace(
    val page: Int,
    val top: Float = 0f,
    val zoom: Float = 1f,
    val panX: Float = 0f,
    val selection: List<UInt>? = null,
)

data class VaultState(
    val root: String? = null,
    /**
     * [root] is the folder just picked, and the core has not handed the vault over yet: the index
     * is being opened, and on the app's first vault the library loaded. Set on the pick itself,
     * so the picker is gone by the next frame rather than sitting there as if the tap were lost.
     */
    val opening: Boolean = false,
    val indexing: Boolean = false,
    /**
     * The reader stopped the walk ([VaultModel.stopIndexing]): the index holds what it had read,
     * and nothing but [VaultModel.resumeIndexing] walks again — not even the rescan every return
     * to the app asks for. Opening the vault again is a resume too, as is Reload Vault.
     */
    val paused: Boolean = false,
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
     * resume starts) leaves it alone, those files still being there to browse. A walk stopped
     * before its first batch sets it only if an earlier walk left files in the index.
     */
    val ready: Boolean = false,
    /** The directories whose children have been listed, so the tree redraws in place. */
    val children: Map<String, List<FileRow>> = emptyMap(),
    val expanded: Set<String> = setOf(""),
    val results: List<SearchHit> = emptyList(),
    val open: Open? = null,
    val pdf: OpenPdf? = null,
    val image: OpenImage? = null,
    /**
     * The PDF the open note was reached from, by a tap on a highlight its link paints, and where
     * the reader was in it: what closing the note goes back to. Anything else opened lets it go.
     */
    val back: OpenPdf? = null,
    val message: String? = null,
) {
    /**
     * Whether the vault opened for [picked] is still wanted: the reader may have closed it, or
     * picked another, while the core was opening it.
     */
    fun waitsFor(picked: String): Boolean = opening && root == picked
}

/**
 * What the switcher ranks: every file, then every note a link names that is not there yet, by the
 * path creating it would give it, then every front matter alias, by the alias.
 *
 * One list, so they rank together, in that order: the ranking is stable, so at the same score a
 * file that is there leads a note only linked to, and a path leads an alias. A row is a place in
 * [names] rather than the string there, because one alias can name two notes.
 */
class Corpus(
    files: List<String> = emptyList(),
    missing: List<String> = emptyList(),
    aliases: List<NoteAlias> = emptyList(),
) {
    /** What the ranking reads. */
    val names = files + missing + aliases.map { it.name }

    private val written = files.size
    private val paths = written + missing.size
    private val notes = aliases.map { it.relPath }

    /** The row the [i]th of [names] stands for. */
    fun row(i: Int): Row = when {
        i < written -> Row(names[i].substringAfterLast('/'), names[i])
        i < paths -> Row(names[i].substringAfterLast('/'), names[i], unwritten = true)
        else -> Row(names[i], notes[i - paths])
    }

    /** Where [rel] is among the paths, if it is: how a recent note finds its row. */
    fun find(rel: String): Int? = names.subList(0, paths).indexOf(rel).takeIf { it >= 0 }

    /**
     * What a row reads and what picking it does: [name] over [rel], and a pick opens [rel], or
     * writes it first when it is [unwritten]. A path's name is its file's; an alias is its own,
     * with the whole path of its note under it, since the alias says nothing of where that is.
     */
    data class Row(val name: String, val rel: String, val unwritten: Boolean = false)
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

    /**
     * Open the vault at [root], leaving the picker on the next frame.
     *
     * The core's open is quick on a warm index, but the first one of a run loads the library and
     * a new vault creates its index; the reader waits for neither on a picker that has not moved.
     * What the core hands back is let go if the reader has closed the vault, or picked another,
     * in the meantime.
     */
    fun open(root: String) {
        _state.value = VaultState(root = root, opening = true)
        viewModelScope.launch {
            val opened = withContext(Dispatchers.IO) { runCatching { Vault.open(root) } }
            if (!_state.value.waitsFor(root)) {
                opened.onSuccess { release(it) }
                return@launch
            }
            opened
                .onSuccess { v ->
                    release(vault)
                    vault = v
                    recents.touch(Recents.Kind.Vaults, root)
                    _state.update { VaultState(root = v.root(), indexing = true) }
                    listen()
                    refresh()
                }
                .onFailure {
                    // Back to the picker, which says why.
                    _state.value = VaultState()
                    fail("Cannot open this folder", it)
                }
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
        var stopped = false
        val touched = mutableSetOf<String>()
        for (event in batch) when (event) {
            is Event.Reconciled -> {
                reindexed = true
                stopped = event.stopped
            }
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
            // The walk is over, so whatever the vault holds is in the index — unless it was
            // stopped, and then the index holds what it had read by then.
            _state.update {
                it.copy(
                    indexing = false,
                    paused = stopped,
                    scanned = 0,
                    phase = null,
                    ready = it.ready || !stopped,
                )
            }
            corpus = null
            relist(_state.value.expanded)
            // Stopped while it was still finding the files, a walk writes nothing, so whether
            // there is anything to browse is whatever an earlier walk left behind.
            if (stopped) {
                _state.update { it.copy(ready = it.ready || !it.children[""].isNullOrEmpty()) }
            }
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
                paused = false,
                // Files found are not files read: past the first walk the status line counts the
                // read ones, and the rescan every return starts runs a scan of its own first.
                scanned = if (p.phase == Phase.SCAN && it.ready) 0 else p.done.toLong(),
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
        corpus?.takeIf { it.names.isNotEmpty() }?.let { return it }
        val v = vault ?: return Corpus()
        // Tens of thousands of strings in one call: worth doing when the switcher opens, which is
        // the only thing that wants them, rather than after every reconcile.
        val read = withContext(Dispatchers.IO) {
            Corpus(
                runCatching { v.filePaths(false) }.getOrDefault(emptyList()),
                runCatching { v.missingNotes() }.getOrDefault(emptyList()),
                runCatching { v.noteAliases() }.getOrDefault(emptyList()),
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
        // A paused vault ignores the rescan, so no walk would come to say it was over.
        if (!_state.value.paused) {
            _state.update { it.copy(indexing = true) }
            runCatching { v.rescan() }
        }
        // An open note may have been changed by Syncthing while the app was away.
        _state.value.open?.let { onChanged(it.rel) }
    }

    /**
     * Reload Vault: a [rescan], except on a paused vault, which refuses every walk but a resume.
     * A reader asking for the vault to be read again there means the rest of it, so it resumes.
     */
    fun reloadVault() {
        if (!_state.value.paused) {
            rescan()
            return
        }
        resumeIndexing()
        _state.value.open?.let { onChanged(it.rel) }
    }

    /**
     * Stop the walk, keeping what it has indexed.
     *
     * Nothing changes here yet: the walk finishes the file it is on and then says it stopped,
     * and that reconcile is what pauses the vault ([apply]). Until it lands, Stop is still Stop.
     */
    fun stopIndexing() = viewModelScope.launch(Dispatchers.IO) {
        runCatching { vault?.stopIndexing() }
    }

    /**
     * Finish a stopped walk. Said at once, as the desktop does: a walk is starting, and a second
     * press would ask for a second one.
     */
    fun resumeIndexing() = viewModelScope.launch(Dispatchers.IO) {
        val v = vault ?: return@launch
        _state.update { it.copy(indexing = true, paused = false) }
        runCatching { v.resumeIndexing() }
    }

    // ------------------------------------------------------------------------------ one file

    /**
     * Open a file, and — for a search hit — say what was being looked for.
     *
     * [find] is the query rather than [SearchHit.at]: the hit's range is bytes into the markdown
     * source and the screen holds the page that source rendered to, so the only thing that
     * survives the crossing is the words. A PDF drops it.
     *
     * [at] is where a PDF opens, and [back] the PDF a note is opened from, for [close] to return
     * to; see [openFromPdf].
     */
    fun openFile(rel: String, find: String? = null, at: PdfPlace? = null, back: OpenPdf? = null) {
        val v = vault ?: return
        // What is in the buffer belongs to the note it was typed into, and the buffer is about to
        // hold another note's text: a write left pending across the swap would put these words in
        // that file.
        leave {
            recents.touch(Recents.Kind.Notes, rel)
            if (rel.endsWith(".pdf", ignoreCase = true)) {
                val path = withContext(Dispatchers.IO) { runCatching { v.pathOf(rel) } }
                path.onSuccess { p ->
                    _state.update {
                        it.copy(pdf = OpenPdf(rel, p, at), open = null, image = null, back = null)
                    }
                }
                    .onFailure { fail("Cannot open this file", it) }
                return@leave
            }
            if (imageKind(rel) != null) {
                val path = withContext(Dispatchers.IO) { runCatching { v.pathOf(rel) } }
                path.onSuccess { p ->
                    _state.update {
                        it.copy(image = OpenImage(rel, p), open = null, pdf = null, back = null)
                    }
                }
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
                        image = null,
                        open = Open(rel, note.text, note.etag, conflicts = conflicts, find = find),
                        back = back,
                    )
                }
            }.onFailure { fail("Cannot read this note", it) }
        }
    }

    /**
     * Put the note down, writing anything the pause was still holding — and go back to the PDF it
     * was reached from, where the reader left it, if a highlight there is what opened it.
     */
    fun close() = leave {
        _state.update { it.copy(open = null, pdf = it.back, image = null, back = null) }
    }

    /**
     * Open the note whose link paints a highlight on the PDF in front, marked at the text the link
     * quotes, as the desktop opens it on a click. The PDF is kept at [place], where the reader
     * was, for Back from the note to return to.
     */
    fun openFromPdf(link: PdfLink, place: PdfPlace) {
        val from = _state.value.pdf ?: return
        openFile(link.srcRelPath, find = link.alias, back = from.copy(at = place, finding = false))
    }

    /**
     * Where on this device the image an embed names is: the path as the note spells it when a file
     * is there, and otherwise the one the index places that name at — `![[img.png]]` is written the
     * way a wikilink is, and the file lives in `Attachments/`. Null for a path out of the vault, or
     * for nothing at all. Blocking: the rendered view asks from its own loading thread.
     */
    fun imagePath(rel: String): String? {
        val v = vault ?: return null
        return runCatching { v.asset(rel)?.let { v.pathOf(it) } }.getOrNull()
    }

    /** The note links into this PDF, which paint as its highlights. */
    suspend fun pdfLinks(rel: String): List<PdfLink> = withContext(Dispatchers.IO) {
        runCatching { vault?.pdfLinks(rel) }.getOrNull().orEmpty()
    }

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
     * a PDF's `page=` after it. The index is what knows which file that is, and failing it the
     * disk, for a file in a tree the index does not hold (a gitignored `build/`); a target
     * neither can place is a link to a note nobody has written yet, which only the switcher
     * writes.
     *
     * A PDF opens on the page the link names, showing the passage its numbers quote there, as the
     * desktop follows one. A heading after a note's name is not placed yet.
     */
    fun openLink(target: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        val name = target.substringBefore('#')
        val found = withContext(Dispatchers.IO) { runCatching { v.follow(name) }.getOrNull() }
        val at = pdfAnchor(target.substringAfter('#', ""))?.let {
            PdfPlace(it.page.toInt(), selection = it.selection)
        }
        when (val rel = found) {
            null -> _state.update { it.copy(message = "No note called \"$name\"") }
            else -> openFile(rel, at = at)
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
     * Open or put away the find of whatever is in front, a note or a PDF.
     *
     * With nothing in front there is nothing to find in and this does nothing, which is what
     * [close] already does from the same palette.
     */
    fun finding(on: Boolean) = _state.update {
        it.copy(open = it.open?.copy(finding = on), pdf = it.pdf?.copy(finding = on))
    }

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
