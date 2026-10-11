package com.xrath.vtamp

import com.xrath.vtamp.model.Track
import com.xrath.vtamp.playback.*
import kotlin.random.Random
import org.junit.Assert.*
import org.junit.Test

class QueueMachineTest {
    private fun track(id: String) = Track(id, id, path = "/$id.m4a", durationMs = 10000)

    private fun queue() =
        QueueMachine(Random(9)).apply { replace(listOf(track("a"), track("b"), track("c")), 0) }

    @Test
    fun duplicateTracksKeepIndependentIdentities() {
        val q = QueueMachine()
        val track = track("a")
        q.replace(listOf(track, track, track), 1)
        assertEquals(3, q.entries.map { it.id }.toSet().size)
        val current = q.currentId
        q.remove(q.entries.first().id)
        assertEquals(current, q.currentId)
        assertEquals(0, q.index)
    }

    @Test
    fun playNowInsertsAndKeepsTheRemainingQueue() {
        val q = queue()
        q.playNow(track("x"))
        assertEquals(listOf("a", "x", "b", "c"), q.entries.map { it.track.id })
        assertEquals("x", q.current!!.track.id)
    }

    @Test
    fun addingToAnEmptyQueueDoesNotStart() {
        val q = QueueMachine()
        q.add(track("a"), true)
        assertNull(q.current)
        assertNull(q.nextId)
    }

    @Test
    fun editsPreserveCurrentIdentity() {
        val q = queue()
        val current = q.currentId!!
        q.move(current, 2)
        assertEquals(current, q.currentId)
        assertEquals(2, q.index)
        q.remove(q.entries.first().id)
        assertEquals(1, q.index)
        assertFalse(q.remove("missing"))
    }

    @Test
    fun removalSelectsFollowerThenPreviousWhenAtEnd() {
        val q = queue()
        q.remove(q.currentId!!)
        assertEquals("b", q.current!!.track.id)
        q.advance()
        q.remove(q.currentId!!)
        assertEquals("b", q.current!!.track.id)
        q.remove(q.currentId!!)
        assertNull(q.current)
    }

    @Test
    fun shuffleVisitsEveryEntryOnceWithoutReordering() {
        val q = queue()
        val visible = q.entries.toList()
        q.shuffle = true
        val played = mutableSetOf(q.currentId)
        while (q.advance()) assertTrue(played.add(q.currentId))
        assertEquals(3, played.size)
        assertEquals(visible, q.entries)
    }

    @Test
    fun additionsAndMovesDoNotReplayVisitedEntries() {
        val q = queue()
        q.shuffle = true
        val played = mutableSetOf(q.currentId)
        q.advance()
        played += q.currentId
        q.add(track("d"))
        q.move(q.entries.last().id, 0)
        while (q.advance()) assertTrue(played.add(q.currentId))
        assertEquals(4, played.size)
    }

    @Test
    fun playNextIsHonoredDuringShuffle() {
        val q = queue()
        q.shuffle = true
        q.add(track("x"), true)
        q.advance()
        assertEquals("x", q.current!!.track.id)
    }

    @Test
    fun repeatOneOnlyAffectsNaturalEnd() {
        val q = queue()
        q.repeat = Repeat.ONE
        val first = q.currentId
        q.advance(true)
        assertEquals(first, q.currentId)
        q.advance()
        assertNotEquals(first, q.currentId)
    }

    @Test
    fun repeatAllMakesAnotherShufflePass() {
        val q = queue()
        q.shuffle = true
        q.repeat = Repeat.ALL
        val firstPass = mutableSetOf(q.currentId)
        repeat(2) {
            q.advance()
            firstPass += q.currentId
        }
        q.advance()
        val secondPass = mutableSetOf(q.currentId)
        repeat(2) {
            q.advance()
            assertTrue(secondPass.add(q.currentId))
        }
        assertEquals(firstPass, secondPass)
    }

    @Test
    fun previousRestartsAfterThreeSecondsAndUsesShuffleHistory() {
        val q = queue()
        q.shuffle = true
        val first = q.currentId
        q.advance()
        val second = q.currentId
        assertFalse(q.previous(3001))
        assertEquals(second, q.currentId)
        assertTrue(q.previous(3000))
        assertEquals(first, q.currentId)
    }

    @Test
    fun radioPreviousNeverRestartsTheStream() {
        val q = queue()
        q.playNow(Track("radio", "Radio", url = "https://example.org/live"))
        assertTrue(q.previous(99999))
        assertEquals("a", q.current!!.track.id)
    }

    @Test
    fun oneEntryShuffleRepeatAndClear() {
        val q = QueueMachine()
        q.replace(listOf(track("a")), 0)
        q.shuffle = true
        assertFalse(q.advance())
        q.repeat = Repeat.ALL
        assertTrue(q.advance())
        q.clear()
        assertNull(q.current)
        assertNull(q.nextId)
        assertTrue(q.entries.isEmpty())
    }
}
