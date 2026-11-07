package io.github.stroblme.accent

import io.github.stroblme.accent.ui.following
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.currentTime
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** The pages follow a moving screen while it moves ([following]), not once it has stopped. */
@OptIn(ExperimentalCoroutinesApi::class) // the virtual clock's reading
class FollowTest {
    @Test
    fun `a fling is followed while it runs and caught up with once it ends`() = runTest {
        val screen = MutableStateFlow(0)
        // When each follow came, where it said the screen was, and where the screen was.
        val followed = mutableListOf<Triple<Long, Int, Int>>()
        backgroundScope.launch {
            screen.following(PERIOD) { screen.value }
                .collect { followed += Triple(currentTime, it, screen.value) }
        }
        // Two seconds of frames, the screen moving a little with each, as a fling's tail does.
        repeat(FRAMES) {
            delay(FRAME)
            screen.value = it + 1
        }
        val moving = followed.filter { it.first <= FRAMES * FRAME }
        assertTrue("followed ${moving.size} times while moving", moving.size >= 10)
        for ((_, said, was) in followed) assertEquals("where the screen was", was, said)
        for ((a, b) in followed.zipWithNext()) {
            assertTrue("no faster than once a period", b.first - a.first >= PERIOD)
        }
        delay(PERIOD + 1)
        assertEquals("caught up a period after it stopped", FRAMES, followed.last().second)
    }

    private companion object {
        const val PERIOD = 180L
        const val FRAME = 16L
        const val FRAMES = 125
    }
}
