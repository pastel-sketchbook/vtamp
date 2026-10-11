package com.xrath.vtamp.playback

import com.xrath.vtamp.model.Track
import java.util.UUID
import kotlin.random.Random

data class QueueEntry(val track: Track, val id: String = UUID.randomUUID().toString())

enum class Repeat {
    OFF,
    ALL,
    ONE,
}

/** Pure queue policy. Identity belongs to entries, never to catalog tracks. */
class QueueMachine(private val random: Random = Random.Default) {
    val entries = mutableListOf<QueueEntry>()
    var currentId: String? = null
        private set

    private val played = mutableSetOf<String>()
    private val history = mutableListOf<String>()
    var shuffle = false
        set(value) {
            field = value
            played.clear()
            currentId?.let(played::add)
            history.clear()
            planned = null
        }

    var repeat = Repeat.OFF
    private var planned: String? = null
    val index
        get() = entries.indexOfFirst { it.id == currentId }

    val current
        get() = entries.getOrNull(index)

    val nextId: String?
        get() {
            if (current == null) return null
            if (!shuffle)
                return entries.getOrNull(index + 1)?.id
                    ?: entries.firstOrNull()?.id?.takeIf { repeat == Repeat.ALL }
            val candidates =
                entries
                    .filter { it.id !in played && it.id != currentId }
                    .ifEmpty {
                        if (repeat == Repeat.ALL)
                            entries.filter { it.id != currentId }.ifEmpty { entries }
                        else emptyList()
                    }
            return planned?.takeIf { id -> candidates.any { it.id == id } }
                ?: candidates.randomOrNull(random)?.id.also { planned = it }
        }

    fun replace(tracks: List<Track>, start: Int) {
        require(start in tracks.indices)
        clear()
        entries += tracks.map(::QueueEntry)
        select(entries[start].id)
    }

    fun playNow(track: Track) {
        val entry = QueueEntry(track)
        entries.add(if (index >= 0) index + 1 else entries.size, entry)
        select(entry.id)
    }

    fun add(track: Track, next: Boolean = false) {
        val entry = QueueEntry(track)
        entries.add(if (next && index >= 0) index + 1 else entries.size, entry)
        // Play Next also takes priority in a shuffled queue.
        if (next && shuffle) planned = entry.id
    }

    fun select(id: String) {
        require(entries.any { it.id == id })
        currentId?.takeIf { it != id }?.let(history::add)
        currentId = id
        played += id
        planned = null
    }

    fun advance(natural: Boolean = false): Boolean {
        if (natural && repeat == Repeat.ONE && current != null) return true
        val next = nextId ?: return false
        if (shuffle && entries.all { it.id in played }) played.clear()
        select(next)
        return true
    }

    /** False means restart the current file; radio has no restart position. */
    fun previous(positionMs: Long): Boolean {
        if (current?.track?.live != true && positionMs > 3000) return false
        val previous = if (shuffle) history.removeLastOrNull() else entries.getOrNull(index - 1)?.id
        if (previous == null) return false
        currentId = previous
        played += previous
        planned = null
        return true
    }

    fun move(id: String, destination: Int) {
        val old = entries.indexOfFirst { it.id == id }
        if (old < 0) return
        val entry = entries.removeAt(old)
        entries.add(destination.coerceIn(0, entries.size), entry)
    }

    /** Returns whether the playing item changed. */
    fun remove(id: String): Boolean {
        val old = entries.indexOfFirst { it.id == id }
        if (old < 0) return false
        entries.removeAt(old)
        played -= id
        history.removeAll { it == id }
        planned = null
        if (currentId != id) return false
        currentId = entries.getOrNull(old)?.id ?: entries.lastOrNull()?.id
        currentId?.let(played::add)
        return true
    }

    fun clear() {
        entries.clear()
        currentId = null
        played.clear()
        history.clear()
        planned = null
    }
}
