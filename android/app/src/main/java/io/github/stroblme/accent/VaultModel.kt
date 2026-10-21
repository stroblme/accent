package io.github.stroblme.accent

import android.app.Application
import android.util.Log
import androidx.compose.foundation.text.input.TextFieldState
import androidx.compose.foundation.text.input.setTextAndPlaceCursorAtEnd
import androidx.compose.runtime.snapshotFlow
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import io.github.stroblme.accent.ffi.AccentException
import io.github.stroblme.accent.ffi.Backlink
import io.github.stroblme.accent.ffi.Etag
import io.github.stroblme.accent.ffi.Event
import io.github.stroblme.accent.ffi.FileRow
import io.github.stroblme.accent.ffi.Note
import io.github.stroblme.accent.ffi.NoteAlias
import io.github.stroblme.accent.ffi.PdfLink
import io.github.stroblme.accent.ffi.Phase
import io.github.stroblme.accent.ffi.Progress
import io.github.stroblme.accent.ffi.Ranker
import io.github.stroblme.accent.ffi.SearchHit
import io.github.stroblme.accent.ffi.TagCount
import io.github.stroblme.accent.ffi.Vault
import io.github.stroblme.accent.ffi.anchorId
import io.github.stroblme.accent.ffi.isAnimated
import io.github.stroblme.accent.ffi.pdfAnchor
import io.github.stroblme.accent.ui.imageKind
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.File
import java.util.concurrent.atomic.AtomicInteger

/** Load note text and reset undo history so undo cannot restore a previous note. */
fun TextFieldState.load(text: String) {
    setTextAndPlaceCursorAtEnd(text)
    undoState.clearHistory()
}

/** The document in front of the reader: a note, a PDF or an image, one at a time. */
sealed interface Doc {
    val rel: String
}

/** What the note in front of the reader is, and what the vault has to say about it. */
data class Open(
    override val rel: String,
    /** Last text read or saved on disk. Compare with [VaultModel.buffer] to detect edits. */
    val text: String = "",
    val etag: Etag? = null,
    /** Somebody else wrote the file while it was open and dirty: autosave has stopped. */
    val changedOnDisk: Boolean = false,
    /** Conflict copies Syncthing left beside it. */
    val conflicts: List<String> = emptyList(),
    /** Search query to highlight once when the opened note appears ([found]). */
    val find: String? = null,
    /** Rendered heading or block ID to visit once when the page appears ([VaultModel.anchored]). */
    val anchor: String? = null,
    /** One-time scroll position for Back or return from editing ([VaultModel.placed]). */
    val scroll: Int = 0,
    /** Whether this note's find bar is open ([VaultModel.finding]). */
    val finding: Boolean = false,
    /** Print… was picked for the note, and its screen has yet to hand it to the print system. */
    val printing: Boolean = false,
    /** Pending exit over unsaved edits after a save conflict ([VaultModel.answer]). */
    val leaving: Boolean = false,
    /** Pending exit after a failed save, with its reason ([VaultModel.answer]). */
    val unsaved: String? = null,
    /** What closing the note goes back to. Anything else opened lets it go. */
    val back: Back? = null,
) : Doc {
    /** Whether [typed] holds anything the vault does not have: what a write would put there. */
    fun dirty(typed: CharSequence): Boolean = typed.toString() != text

    /** Whether leaving would lose [typed] edits while saving is paused. */
    fun wouldLose(typed: CharSequence): Boolean = changedOnDisk && dirty(typed)
}

/** Open PDF path, starting position, find state, and pending print request. */
data class OpenPdf(
    override val rel: String,
    val path: String,
    val at: PdfPlace? = null,
    val finding: Boolean = false,
    val printing: Boolean = false,
) : Doc

/** Open image path and animation flag. Animated images cannot be recoloured or inverted. */
data class OpenImage(override val rel: String, val path: String, val animated: Boolean) : Doc

/** [rel] at [path], read for whether it moves. Blocking. */
private fun imageAt(rel: String, path: String) = OpenImage(rel, path, isAnimated(path))

/** The document a note was reached from, which Back from the note returns to. */
sealed interface Back {
    /** A PDF, by a tap on a highlight the note's link paints, kept where the reader was in it. */
    data class Pdf(val pdf: OpenPdf) : Back

    /** Reopen a backlink source at its last PDF or note position; images open fitted. */
    data class File(val rel: String, val at: PdfPlace? = null, val scroll: Int = 0) : Back
}

/** PDF page, offset, zoom, pan, or selected passage for an internal link. */
data class PdfPlace(
    val page: Int,
    val top: Float = 0f,
    val zoom: Float = 1f,
    val panX: Float = 0f,
    val selection: List<UInt>? = null,
)

data class VaultState(
    val root: String? = null,
    /** Vault [root] is opening; show loading immediately while the core initializes. */
    val opening: Boolean = false,
    val indexing: Boolean = false,
    /** Index walk paused by the reader; only resume or reopening starts another walk. */
    val paused: Boolean = false,
    /** Files the walk has been through so far, for as long as one is running. */
    val scanned: Long = 0,
    /** Index phase. During [Phase.SCAN], no files are available to browse. */
    val phase: Phase? = null,
    /** Latch once indexed files are available; retain during rescans. */
    val ready: Boolean = false,
    /**
     * Revision incremented as indexing publishes batches or finishes, triggering list refreshes.
     */
    val revision: Int = 0,
    /** The directories whose children have been listed, so the tree redraws in place. */
    val children: Map<String, List<FileRow>> = emptyMap(),
    val expanded: Set<String> = setOf(""),
    val results: List<SearchHit> = emptyList(),
    /** Query for [results], retained while the next search runs. */
    val searched: String = "",
    /** Why [searched] could not be searched, said where [results] would say nothing matched. */
    val searchFailed: String? = null,
    val doc: Doc? = null,
    val message: String? = null,
    /** The vault's worker is gone ([Event.WorkerGone]): nothing reads the vault until it reopens. */
    val gone: Boolean = false,
) {
    /** The note in front, if a note is. */
    val note: Open? get() = doc as? Open

    /** [f] applied to the note in front, if it is [rel] when that is given; anything else stays. */
    fun withNote(rel: String? = null, f: (Open) -> Open): VaultState {
        val now = note?.takeIf { rel == null || it.rel == rel } ?: return this
        return copy(doc = f(now))
    }

    /** Find opened or closed on the note or PDF in front; an image has none. */
    fun finding(on: Boolean): VaultState = copy(
        doc = when (doc) {
            is Open -> doc.copy(finding = on)
            is OpenPdf -> doc.copy(finding = on)
            else -> doc
        },
    )

    /** Print asked for, or handed over, on the note or PDF in front; an image has none. */
    fun printing(on: Boolean): VaultState = copy(
        doc = when (doc) {
            is Open -> doc.copy(printing = on)
            is OpenPdf -> doc.copy(printing = on)
            else -> doc
        },
    )

    /** The document in front closed: back to the PDF the note was reached from, else nothing. */
    fun closed(): VaultState = copy(doc = (note?.back as? Back.Pdf)?.pdf)

    /** [query] searched: [hits], or why there are none to show. */
    fun answered(query: String, hits: Result<List<SearchHit>>): VaultState = copy(
        results = hits.getOrDefault(emptyList()),
        searched = query,
        searchFailed = hits.exceptionOrNull()?.let { failure("Cannot search", it) },
    )

    /** Whether the still-opening vault for [picked] is still wanted. */
    fun waitsFor(picked: String): Boolean = opening && root == picked

    /** Clear indexing state after failure or interrupted event reading; retain indexed files. */
    fun walkOver(): VaultState = copy(indexing = false, paused = false, scanned = 0, phase = null)

    /** The walk over for good, said until the vault is opened again. */
    fun workerGone(): VaultState = walkOver().copy(gone = true)
}

/** Switcher names in tie-break order: indexed, ignored, missing linked, then aliases. */
class Corpus(
    files: List<String> = emptyList(),
    ignored: List<String> = emptyList(),
    missing: List<String> = emptyList(),
    aliases: List<NoteAlias> = emptyList(),
) {
    /** The ignored notes the index does not hold already, as the desktop joins them: once. */
    private val unindexed = ignored - files.toSet()

    /** What the ranking reads. */
    val names = files + unindexed + missing + aliases.map { it.name }

    /** Core-ranked switcher names loaded off the main thread on first use. */
    val ranker: Ranker by lazy { Ranker(names) }

    private val indexed = files.size
    private val written = indexed + unindexed.size
    private val paths = written + missing.size
    private val notes = aliases.map { it.relPath }

    /** The row the [i]th of [names] stands for. */
    fun row(i: Int): Row = when {
        i < indexed -> Row(names[i].substringAfterLast('/'), names[i])
        i < written -> Row(names[i].substringAfterLast('/'), names[i], ignored = true)
        i < paths -> Row(names[i].substringAfterLast('/'), names[i], unwritten = true)
        else -> Row(names[i], notes[i - paths])
    }

    /** Where [rel] is among the paths, if it is: how a recent note finds its row. */
    fun find(rel: String): Int? = names.subList(0, paths).indexOf(rel).takeIf { it >= 0 }

    /** Switcher row label and target [rel]; create [unwritten] notes when picked. */
    data class Row(
        val name: String,
        val rel: String,
        val unwritten: Boolean = false,
        val ignored: Boolean = false,
    )
}

/** Coalesce requests during a name load into one follow-up load instead of restarting it. */
class Reload {
    private var running = false

    /** Asked for again while running, and whether with the ignored folders walked. */
    private var again: Boolean? = null

    /** A load is asked for, [walk] saying whether it walks the ignored folders: start it now? */
    fun ask(walk: Boolean): Boolean {
        if (!running) {
            running = true
            return true
        }
        again = again == true || walk
        return false
    }

    /** A load has ended: another to run after it, and whether it walks; null for none. */
    fun next(): Boolean? = again.also {
        again = null
        running = it != null
    }
}

/** Tickets for requests of which only the latest counts: an answer to an earlier one is dropped. */
class Latest {
    private val asked = AtomicInteger()

    /** A request, superseding every one before it. */
    fun next(): Int = asked.incrementAndGet()

    /** Whether [ticket]'s request is still the latest. */
    fun holds(ticket: Int): Boolean = asked.get() == ticket
}

/**
 * What the note [asked] about becomes once its file is [read] again, [now] being the note in front
 * by then and [typed] what the buffer holds; `null` when another note or a save has landed
 * meanwhile. Dropping is safe: a write it overtook is caught by the next save's etag check.
 */
fun reread(asked: Open, now: Open?, read: Note, typed: CharSequence): Open? {
    if (now == null || now.rel != asked.rel || now.etag != asked.etag) return null
    return when {
        // Only touched: the text is the one already here.
        now.etag != null && now.text == read.text -> now.copy(etag = read.etag)
        // Nothing was typed here, so taking the new text loses nothing.
        now.text != read.text && !now.dirty(typed) ->
            now.copy(text = read.text, etag = read.etag, changedOnDisk = false)
        else -> now.copy(changedOnDisk = true)
    }
}

/** What came of keeping the other copy of a note ([takeTheirs]). */
sealed interface Theirs {
    /** The note holds the copy's text, and the copy is gone. */
    data object Taken : Theirs

    /** The note holds the copy's text, but the copy is still there, for [why]. */
    data class CopyKept(val why: Throwable) : Theirs

    /** Nothing was taken, and nothing removed, for [why]. */
    data class Failed(val why: Throwable) : Theirs
}

/** Take a conflict copy as its note ([adopt]), and only once that is done [remove] the copy. */
fun takeTheirs(adopt: () -> Unit, remove: () -> Unit): Theirs {
    runCatching(adopt).onFailure { return Theirs.Failed(it) }
    return runCatching(remove).fold({ Theirs.Taken }, { Theirs.CopyKept(it) })
}

/** [what] could not be done, with the core's reason when it gave one. */
fun failure(what: String, e: Throwable): String {
    val why = (e as? AccentException.Failed)?.reason ?: e.message
    return if (why.isNullOrBlank()) what else "$what: $why"
}

/** Deduplicate [links] by source note in indexed path order. */
fun linkingNotes(links: List<Backlink>): List<String> = links.map { it.srcRelPath }.distinct()

/** Return to [rel] at [here] only if that position belongs to the same document. */
fun backTo(rel: String, here: Back.File?): Back.File =
    here?.takeIf { it.rel == rel } ?: Back.File(rel)

/** Save against [expected]. If the etag changed but text still equals [last], retry the save. */
fun saveOver(expected: Etag?, last: String, read: () -> Note, save: (Etag?) -> Etag): Etag =
    try {
        save(expected)
    } catch (e: AccentException.ChangedOnDisk) {
        val now = runCatching(read).getOrNull()?.takeIf { it.text == last } ?: throw e
        save(now.etag)
    }

/**
 * Vault state and open note. Blocking core calls use [Dispatchers.IO]; filesystem updates use [rescan].
 */
class VaultModel(app: Application) : AndroidViewModel(app) {
    private val _state = MutableStateFlow(VaultState())
    val state: StateFlow<VaultState> = _state.asStateFlow()

    val recents = Recents(app)

    /** Hoisted editor buffer shared by rendering, saving, and conflict resolution. */
    val buffer = TextFieldState()

    private var vault: Vault? = null

    /** The exit [leave] is holding while the reader answers, if there is one. */
    private var held: (suspend () -> Unit)? = null

    /** One [write] at a time, so each goes out with the etag the one before it recorded. */
    private val writing = Mutex()

    /** The file asked for last: a read for an earlier one that lands later is dropped ([show]). */
    private val shown = Latest()

    /** Switcher names, `null` until first load; ignored notes join when scanned. */
    private val _names = MutableStateFlow<Corpus?>(null)
    val names: StateFlow<Corpus?> = _names.asStateFlow()

    /** The notes in the folders git ignores, as last walked, kept while the index's are reread. */
    @Volatile private var ignored: List<String> = emptyList()
    private val reload = Reload()

    /** Those folders are being walked: the switcher's list may still grow. */
    private val _listing = MutableStateFlow(false)
    val listing: StateFlow<Boolean> = _listing.asStateFlow()

    /** How far the batch being applied said the walk had got, and when the tree last caught up. */
    private var seen: Progress? = null
    private var lastRelist = 0L

    init {
        // Autosave: a pause in the typing, not a queue of them — `collectLatest` drops the wait
        // the moment the next keystroke lands, though not a write already under way ([write]).
        // It runs for as long as the model does rather than for as long as the editor is
        // composed, so leaving the editor inside that second no longer needs a write of its own,
        // and what it is compared against is read when the wait is over: typing a word and
        // taking it back again saves nothing, and a note replaced under it (a reload, another
        // note) is left alone.
        viewModelScope.launch {
            snapshotFlow { buffer.text.toString() }.collectLatest { text ->
                delay(SAVE_AFTER_MS)
                tell(write(text))
            }
        }
        // The foreground service a walk holds ([IndexingService]): started as one starts, its
        // count kept, and stopped as it ends.
        viewModelScope.launch {
            var walking = false
            state.map { s ->
                s.root?.takeIf { s.indexing }?.let { indexingText(File(it).name, s.scanned) }
            }.distinctUntilChanged().collect { text ->
                when {
                    text == null -> IndexingService.stop(app)
                    walking -> IndexingService.update(app, text)
                    else -> IndexingService.start(app, text)
                }
                walking = text != null
            }
        }
    }

    /**
     * Open [root] asynchronously, leaving the picker immediately and discarding superseded results.
     */
    fun open(root: String) {
        shown.next()
        _state.value = VaultState(root = root, opening = true)
        _names.value = null
        ignored = emptyList()
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
        try {
            while (isActive && vault === v) {
                // Half a second is short enough that closing the vault is not felt and long
                // enough that a quiet app is not waking up for nothing.
                val batch = runCatching { v.nextEvents(500u) }.getOrElse { return@launch }
                if (batch.isNotEmpty()) apply(batch)
            }
        } finally {
            // However the loop ends, nothing will say a walk is over any more. A vault closed or
            // replaced meanwhile has a state of its own.
            if (vault === v) _state.update { it.walkOver() }
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
            // Over, the progress it had made with it, and not paused. Its error, if this vault has
            // not said it already, is an [Event.Error] of its own.
            Event.WalkFailed -> {
                seen = null
                _state.update { it.walkOver() }
            }
            // The same, its reason an [Event.Error] too, and for good: the banner asks to reopen.
            Event.WorkerGone -> {
                seen = null
                _state.update { it.workerGone() }
            }
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
                    revision = it.revision + 1,
                )
            }
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

    /** Publish walk progress and refresh the tree after each indexed batch. */
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
        _state.update { it.copy(revision = it.revision + 1) }
    }

    /** Reload switcher names without cancelling an active load; optionally scan ignored folders. */
    fun loadNames(walkIgnored: Boolean) {
        if (!reload.ask(walkIgnored)) return
        viewModelScope.launch {
            var walk: Boolean? = walkIgnored
            while (walk != null) {
                load(walk)
                walk = reload.next()
            }
        }
    }

    private suspend fun load(walk: Boolean) {
        val v = vault ?: return
        // The ranker's copy is made here too, off the main thread, rather than at the first key.
        fun publish(files: List<String>, missing: List<String>, aliases: List<NoteAlias>) {
            val read = Corpus(files, ignored, missing, aliases).also { it.ranker }
            if (vault === v) _names.value = read
        }
        // Not read, the names already listed stay: an empty switcher would say the vault is.
        val (files, missing, aliases) = withContext(Dispatchers.IO) {
            runCatching { Triple(v.filePaths(false), v.missingNotes(), v.noteAliases()) }
                .onSuccess { (f, m, a) -> publish(f, m, a) }
                .onFailure { logged("Cannot list the notes", it) }
                .getOrNull()
        } ?: return
        if (!walk) return
        _listing.value = true
        try {
            withContext(Dispatchers.IO) {
                val walked = runCatching { v.ignoredNotes(true) }
                    .onFailure { logged("Cannot list the ignored notes", it) }
                    .getOrNull() ?: return@withContext
                if (vault !== v) return@withContext
                ignored = walked
                publish(files, missing, aliases)
            }
        } finally {
            _listing.value = false
        }
    }

    /** List every directory the tree has open again. */
    fun refresh() = viewModelScope.launch { relist(_state.value.expanded) }

    private suspend fun relist(dirs: Set<String>) = withContext(Dispatchers.IO) {
        val v = vault ?: return@withContext
        val listed = dirs.mapNotNull { dir ->
            runCatching { dir to v.listDir(dir) }
                .onFailure { logged("Cannot list \"$dir\"", it) }
                .getOrNull()
        }
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
        // A paused vault ignores the rescan, so no walk would come to say it was over. A running
        // walk is reading the vault already, and asked again the core would walk the whole of it
        // once more the moment this one ended. Asked and marked in one step, so two returns in a
        // row ask once.
        val was = _state.getAndUpdate {
            if (it.paused || it.indexing) it else it.copy(indexing = true)
        }
        // Not asked, no walk will say it is over either.
        if (!was.paused && !was.indexing) runCatching { v.rescan() }.onFailure {
            if (vault === v) _state.update { s -> s.walkOver() }
            fail("Cannot read the vault again", it)
        }
        // An open note may have been changed by Syncthing while the app was away.
        _state.value.note?.let { onChanged(it.rel) }
    }

    /** Rescan the vault, or resume indexing if it was paused. */
    fun reloadVault() {
        if (!_state.value.paused) {
            rescan()
            return
        }
        resumeIndexing()
        _state.value.note?.let { onChanged(it.rel) }
    }

    /** Request a stop; pause after the active file completes, retaining indexed files. */
    fun stopIndexing() = viewModelScope.launch(Dispatchers.IO) {
        runCatching { vault?.stopIndexing() }
    }

    /** Resume a paused walk and publish its starting state immediately. */
    fun resumeIndexing() = viewModelScope.launch(Dispatchers.IO) {
        val v = vault ?: return@launch
        _state.update { it.copy(indexing = true, paused = false) }
        runCatching { v.resumeIndexing() }.onFailure {
            if (vault === v) _state.update { s -> s.walkOver() }
            fail("Cannot resume reading the vault", it)
        }
    }

    // ------------------------------------------------------------------------------ one file

    /** Open a file and pass a search query for one-time highlighting. PDFs ignore the query. */
    fun openFile(
        rel: String, find: String? = null, at: PdfPlace? = null, back: Back? = null,
        anchor: String? = null,
    ) {
        // Asked for now, not once the buffer is written: a file asked for after this one wins,
        // whichever read ends first.
        val ticket = shown.next()
        // What is in the buffer belongs to the note it was typed into, and the buffer is about to
        // hold another note's text: a write left pending across the swap would put these words in
        // that file.
        leave { show(rel, ticket, find, at, back, anchor = anchor) }
    }

    /**
     * Put the file in front, whatever the buffer holds: [openFile] once [leave] has written it.
     * Nothing changes unless [ticket] is still the latest request ([shown]) when the file is read.
     */
    private suspend fun show(
        rel: String,
        ticket: Int,
        find: String? = null,
        at: PdfPlace? = null,
        back: Back? = null,
        scroll: Int = 0,
        anchor: String? = null,
    ) {
        val v = vault ?: return
        val pdf = rel.endsWith(".pdf", ignoreCase = true)
        val image = imageKind(rel) != null
        val typed = buffer.text.toString()
        val read = withContext(Dispatchers.IO) {
            runCatching {
                when {
                    pdf -> OpenPdf(rel, v.pathOf(rel), at)
                    image -> imageAt(rel, v.pathOf(rel))
                    else -> v.read(rel).let {
                        // Without them the note still opens; the banner waits for the next look.
                        val conflicts = runCatching { v.conflictsOf(rel) }
                            .onFailure { e -> logged("Cannot look for conflict copies", e) }
                            .getOrDefault(emptyList())
                        Open(
                            rel, it.text, it.etag,
                            conflicts = conflicts, find = find, scroll = scroll, back = back,
                            anchor = anchor?.let { name -> anchorId(it.text, name) },
                        )
                    }
                }
            }
        }
        // Asked for something else since, or closed the vault: the read is not wanted, nor is
        // its failure.
        fun current() = shown.holds(ticket) && vault === v
        if (!current()) return
        val doc = read.getOrElse {
            return fail(if (pdf || image) "Cannot open this file" else "Cannot read this note", it)
        }
        val swap: suspend () -> Unit = {
            if (current()) {
                recents.touch(Recents.Kind.Notes, rel)
                // The buffer and the state in one step, so nothing composes a note's name over
                // another note's text.
                if (doc is Open) buffer.load(doc.text)
                _state.update { it.copy(doc = doc) }
                viewModelScope.launch(Dispatchers.IO) {
                    runCatching { v.keepOpened(rel) }
                        .onFailure { logged("Cannot keep this file in search", it) }
                }
            }
        }
        // Words typed while the file was read are still waiting out their autosave, which the
        // swap would cancel: written first, as [leave] writes the rest.
        if (buffer.text.toString() != typed) exit(flush(), swap) else swap()
    }

    /** Close the note after saving pending edits, then return to its backlink source if any. */
    fun close() {
        val ticket = shown.next()
        leave {
            when (val to = _state.value.note?.back) {
                // [show], not [openFile]: the buffer has been dealt with, and going through
                // [leave] again would ask again about edits the reader has just chosen to drop.
                is Back.File -> show(to.rel, ticket, at = to.at, scroll = to.scroll)
                else -> if (shown.holds(ticket)) _state.update { it.closed() }
            }
        }
    }

    /** Open a PDF highlight's source note and save the PDF position for Back. */
    fun openFromPdf(link: PdfLink, place: PdfPlace) {
        val from = _state.value.doc as? OpenPdf ?: return
        val back = Back.Pdf(from.copy(at = place, finding = false))
        openFile(link.srcRelPath, find = link.alias, back = back)
    }

    /** Last document position for returning from a backlink; kept outside UI state. */
    private var here: Back.File? = null

    /**
     * The screen in front saying where the reader is in [rel]: [at] in a PDF, [scroll] pixels
     * down a note.
     */
    fun reading(rel: String, at: PdfPlace? = null, scroll: Int = 0) {
        here = Back.File(rel, at, scroll)
    }

    /**
     * Open [note], one of the notes linking to [from], for Back to open [from] again where the
     * reader left it.
     */
    fun openBacklink(note: String, from: String) = openFile(note, back = backTo(from, here))

    /** Resolve an embedded image within the vault, including indexed wikilink targets. */
    fun image(rel: String): OpenImage? {
        val v = vault ?: return null
        return runCatching { v.asset(rel)?.let { imageAt(it, v.pathOf(it)) } }.getOrNull()
    }

    /** The notes that link to the document at [rel], once each ([linkingNotes]). */
    suspend fun linkedFrom(rel: String): Result<List<String>> =
        listing("Cannot list the links") { linkingNotes(vault?.backlinks(rel).orEmpty()) }

    /** Every tag in the vault with how many notes carry it, most used first. */
    suspend fun tags(): Result<List<TagCount>> =
        listing("Cannot list the tags") { vault?.tags().orEmpty() }

    /** The notes carrying [tag], by path. */
    suspend fun filesWithTag(tag: String): Result<List<String>> =
        listing("Cannot list the tag's notes") {
            vault?.filesWithTag(tag).orEmpty().map { it.relPath }
        }

    /**
     * A list Browse shows, read off the main thread. A failure is logged as [what], and handed on
     * for the page to say where its rows would be.
     */
    private suspend fun <T> listing(what: String, read: () -> List<T>): Result<List<T>> =
        withContext(Dispatchers.IO) { runCatching(read).onFailure { logged(what, it) } }

    /**
     * Templates with `accent-target:` offered by New from Template…; `null` without a vault, or
     * when they cannot be listed, which is said.
     */
    suspend fun templateTargets(): List<String>? = withContext(Dispatchers.IO) {
        val v = vault ?: return@withContext null
        runCatching { v.templateTargets() }
            .onFailure { fail("Cannot list the templates", it) }
            .getOrNull()
    }

    /**
     * Templates with a dated `accent-target:` for Today's Note, as [templateTargets] lists them.
     * One whose destination cannot be read is left out, not the rest with it.
     */
    suspend fun dailyTemplates(): List<String>? = withContext(Dispatchers.IO) {
        val v = vault ?: return@withContext null
        templateTargets()?.filter { template ->
            runCatching { v.templateRawTarget(template) }
                .onFailure { logged("Cannot read where $template puts its notes", it) }
                .getOrNull()?.contains("{{date") == true
        }
    }

    /** Where the templates are read from, for the reader told there are none to offer. */
    fun templatesDir(): String? = vault?.templatesDir()

    /** The note links into this PDF, which paint as its highlights; `null` if not read. */
    suspend fun pdfLinks(rel: String): List<PdfLink>? = withContext(Dispatchers.IO) {
        runCatching { vault?.pdfLinks(rel).orEmpty() }
            .onFailure { logged("Cannot read the links into this PDF", it) }
            .getOrNull()
    }

    /** Open the vault again, its note written first: a new worker after [Event.WorkerGone]. */
    fun reopen() {
        val root = _state.value.root ?: return
        leave { open(root) }
    }

    /** Close the vault, retain it in recents, and release its state and handle. */
    fun closeVault() {
        shown.next()
        leave {
            // After the buffer is written rather than before, because the write goes through the
            // handle: the buffer is the only copy of whatever was typed in the last second.
            release(vault)
            vault = null
            _names.value = null
            ignored = emptyList()
            _state.value = VaultState()
        }
    }

    /** Follow a note link through the index or disk, opening its heading, block or PDF passage. */
    fun openLink(target: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        val ticket = shown.next()
        val name = target.substringBefore('#')
        val found = withContext(Dispatchers.IO) { runCatching { v.follow(name) } }
        // Another file asked for while the link was looked up goes in front instead.
        if (!shown.holds(ticket) || vault !== v) return@launch
        found.onFailure { return@launch fail("Cannot follow this link", it) }
        val at = pdfAnchor(target.substringAfter('#', ""))?.let {
            PdfPlace(it.page.toInt(), selection = it.selection)
        }
        val anchor = target.substringAfter('#', "").ifEmpty { null }
        when (val rel = found.getOrNull()) {
            null -> _state.update { it.copy(message = "No note called \"$name\"") }
            else -> openFile(rel, at = at, anchor = anchor)
        }
    }

    /** What came of a [write]. */
    private sealed interface Saved {
        /** The vault has the text: written now, or nothing to write. */
        data object Written : Saved

        /** Not written, because the file moved under the note: saving is paused, the banner up. */
        data object Paused : Saved

        /** Not written, for [why], worded as [fail] words it. */
        data class Failed(val why: String) : Saved
    }

    /** Save the buffer, serializing writes; preserve it and show a banner on content conflicts. */
    private suspend fun write(text: String, force: Boolean = false): Saved = writing.withLock {
        val v = vault ?: return@withLock Saved.Written
        val open = _state.value.note ?: return@withLock Saved.Written
        if (!force && !open.dirty(text)) return@withLock Saved.Written
        if (!force && open.wouldLose(text)) return@withLock Saved.Paused
        withContext(NonCancellable) {
            val written = withContext(Dispatchers.IO) {
                runCatching {
                    saveOver(if (force) null else open.etag, open.text, { v.read(open.rel) }) {
                        v.save(open.rel, text, it)
                    }
                }
            }
            written.fold(
                onSuccess = { etag ->
                    // What was just written is what is on disk, so there is nothing left to
                    // choose between.
                    if (vault === v) _state.update { s ->
                        s.withNote(open.rel) {
                            it.copy(text = text, etag = etag, changedOnDisk = false)
                        }
                    }
                    Saved.Written
                },
                onFailure = { e ->
                    when (e) {
                        is AccentException.ChangedOnDisk -> {
                            if (vault === v) _state.update { s ->
                                s.withNote(open.rel) { it.copy(changedOnDisk = true) }
                            }
                            Saved.Paused
                        }
                        else -> Saved.Failed(logged("Cannot save this note", e))
                    }
                },
            )
        }
    }

    /**
     * Save before an exit can discard the buffer; leave paused conflicts for the reader to resolve.
     */
    private suspend fun flush(): Saved = write(buffer.text.toString())

    /**
     * Take the buffer away — for another note, for none, or with the vault — once [flush] has
     * written it.
     */
    private fun leave(then: suspend () -> Unit) = viewModelScope.launch { exit(flush(), then) }

    /** Exit when [saved]; otherwise hold the exit and ask about edits that cannot be saved. */
    private suspend fun exit(saved: Saved, then: suspend () -> Unit) {
        when (saved) {
            Saved.Written -> then()
            Saved.Paused -> {
                held = then
                _state.update { s -> s.withNote { it.copy(leaving = true) } }
            }
            is Saved.Failed -> {
                held = then
                _state.update { s -> s.withNote { it.copy(unsaved = saved.why) } }
            }
        }
    }

    /**
     * Resolve a pending [exit]: keep edits, discard them, or stay. Failed saves cannot keep edits.
     */
    fun answer(keep: Boolean?) = viewModelScope.launch {
        val then = held ?: return@launch
        held = null
        _state.update { s -> s.withNote { it.copy(leaving = false, unsaved = null) } }
        when (keep) {
            null -> {}
            true -> exit(write(buffer.text.toString(), force = true), then)
            false -> then()
        }
    }

    /** Keep mine: write the buffer over the version on disk. */
    fun overwrite() = viewModelScope.launch { tell(write(buffer.text.toString(), force = true)) }

    /** Say why a write failed, where nothing is waiting on it to ask instead. */
    private fun tell(saved: Saved) {
        if (saved is Saved.Failed) said(saved.why)
    }

    /** Replace the buffer with disk text, discarding local edits without another save attempt. */
    fun reload() {
        val open = _state.value.note ?: return
        val ticket = shown.next()
        viewModelScope.launch { show(open.rel, ticket, back = open.back) }
    }

    private suspend fun makeNote(v: Vault, rel: String, ticket: Int) {
        val made = withContext(Dispatchers.IO) { runCatching { v.createNote(rel, null) } }
        if (!shown.holds(ticket) || vault !== v) return
        made.onSuccess { leave { show(rel, ticket) } }
            .onFailure { fail("Cannot create this note", it) }
    }

    fun newNote(rel: String) {
        val v = vault ?: return
        val ticket = shown.next()
        viewModelScope.launch { makeNote(v, rel, ticket) }
    }

    /** Create or open the note at [template]'s target, including an existing daily note. */
    fun fromTemplate(template: String) {
        val v = vault ?: return
        val ticket = shown.next()
        viewModelScope.launch {
            val made = withContext(Dispatchers.IO) { runCatching { v.noteFromTemplate(template) } }
            if (!shown.holds(ticket) || vault !== v) return@launch
            made.onSuccess { rel ->
                // The file changed since the list was read; nothing was made.
                if (rel == null) return@onSuccess said("This template no longer has a destination")
                leave { show(rel, ticket) }
            }.onFailure { fail("Cannot create this note", it) }
        }
    }

    /** Create a missing linked note or open it if another writer created it first. */
    fun create(rel: String) {
        val v = vault ?: return
        val ticket = shown.next()
        viewModelScope.launch {
            val exists = withContext(Dispatchers.IO) { v.exists(rel) }
            if (!shown.holds(ticket) || vault !== v) return@launch
            if (exists) leave { show(rel, ticket) } else makeNote(v, rel, ticket)
        }
    }

    // ----------------------------------------------------------------------- sync conflicts

    /**
     * Take the conflict copy as the note and delete it, once [leave] has written what was typed:
     * the text it replaces is what the core keeps as a conflict copy of its own, to be taken back.
     */
    fun keepTheirs(conflict: String) {
        val open = _state.value.note ?: return
        val ticket = shown.next()
        leave {
            val v = vault ?: return@leave
            val taken = withContext(Dispatchers.IO) {
                takeTheirs({ v.adoptConflict(open.rel, conflict) }, { v.delete(conflict) })
            }
            when (taken) {
                is Theirs.Failed -> return@leave fail("Cannot take the other copy", taken.why)
                is Theirs.CopyKept -> fail("Took the other copy, but cannot remove it", taken.why)
                Theirs.Taken -> {}
            }
            show(open.rel, ticket, back = open.back)
            // The index has yet to hear that the copy went, so the banner is told, as on the
            // desktop.
            if (taken == Theirs.Taken) _state.update { s ->
                s.withNote(open.rel) { it.copy(conflicts = it.conflicts - conflict) }
            }
        }
    }

    /** Keep this note as it stands and delete the conflict copy. */
    fun keepMine(conflict: String) = viewModelScope.launch {
        val v = vault ?: return@launch
        val done = withContext(Dispatchers.IO) { runCatching { v.delete(conflict) } }
        done.onSuccess {
            _state.update { s -> s.withNote { it.copy(conflicts = it.conflicts - conflict) } }
        }.onFailure { fail("Cannot remove the other copy", it) }
    }

    private fun onConflict(original: String) = _state.update { s ->
        s.withNote(original) { it.copy(conflicts = emptyList()) }
    }.also { _state.value.note?.let { open -> refreshConflicts(open.rel) } }

    private fun refreshConflicts(rel: String) = viewModelScope.launch(Dispatchers.IO) {
        val v = vault ?: return@launch
        // Not read, the banner keeps what it shows.
        val found = runCatching { v.conflictsOf(rel) }
            .onFailure { logged("Cannot look for conflict copies", it) }
            .getOrNull() ?: return@launch
        _state.update { s -> s.withNote(rel) { it.copy(conflicts = found) } }
    }

    // ------------------------------------------------------------------------------- search

    /** Search generation; ignore superseded responses, including errors. */
    private val asked = Latest()

    fun search(query: String) {
        val mine = asked.next()
        viewModelScope.launch(Dispatchers.IO) {
            val v = vault ?: return@launch
            val hits = if (query.isBlank()) Result.success(emptyList())
            else runCatching { v.search(query, 100u, false) }
                .onFailure { logged("Cannot search", it) }
            _state.update { if (!asked.holds(mine)) it else it.answered(query, hits) }
        }
    }

    /** The query has been marked on the page. One-shot, the same way [said] is. */
    fun found() = _state.update { s -> s.withNote { it.copy(find = null) } }

    /** The rendered note has visited the requested heading or block. */
    fun anchored(rel: String, anchor: String) = _state.update { s ->
        s.withNote(rel) { if (it.anchor == anchor) it.copy(anchor = null) else it }
    }

    /** The rendered note has been put back where the reader was ([Open.scroll]). */
    fun placed() = _state.update { s -> s.withNote { it.copy(scroll = 0) } }

    /** Save rendered-note position across an editor visit. */
    fun keepPlace() = _state.update { s ->
        s.withNote { it.copy(scroll = backTo(it.rel, here).scroll) }
    }

    /** Toggle find for the open note or PDF; do nothing with no document open. */
    fun finding(on: Boolean) = _state.update { it.finding(on) }

    /** Toggle printing for the open note or PDF; its screen performs the print. */
    fun printing(on: Boolean) = _state.update { it.printing(on) }

    fun said(message: String?) = _state.update { it.copy(message = message) }

    // ------------------------------------------------------------------------------ helpers

    /** Reload a clean open note after external changes; show a conflict for local edits. */
    private fun onChanged(rel: String) {
        val v = vault ?: return
        val asked = _state.value.note?.takeIf { it.rel == rel } ?: return
        refreshConflicts(rel)
        viewModelScope.launch {
            val read = withContext(Dispatchers.IO) {
                runCatching { v.read(rel) }.onFailure { logged("Cannot read the note again", it) }
            }
            val note = read.getOrNull() ?: return@launch
            if (vault !== v) return@launch
            val now = _state.value.note
            val next = reread(asked, now, note, buffer.text) ?: return@launch
            // The buffer and the state in one step, as a note is opened. The conflicts are
            // refreshed off this thread meanwhile, so they are kept as they now stand.
            if (next.text != now?.text) buffer.load(next.text)
            _state.update { s -> s.withNote(rel) { next.copy(conflicts = it.conflicts) } }
        }
    }

    /**
     * Release the vault off the UI thread; core drop may wait for indexing and language sessions.
     */
    private fun release(handle: Vault?) {
        val going = handle ?: return
        Thread({ going.close() }, "vault-close").start()
    }

    /** Say why [what] could not be done, and log it. */
    private fun fail(what: String, e: Throwable) = said(logged(what, e))

    /** Log why [what] could not be done; the words [failure] gives it, for whoever says them. */
    private fun logged(what: String, e: Throwable): String =
        failure(what, e).also { Log.w(Native.TAG, it, e) }

    override fun onCleared() {
        IndexingService.stop(getApplication())
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
