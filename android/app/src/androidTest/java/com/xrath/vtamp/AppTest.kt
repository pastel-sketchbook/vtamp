@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package com.xrath.vtamp

import android.content.*
import android.os.IBinder
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.lifecycle.Lifecycle
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.xrath.vtamp.playback.*
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AppTest {
    @get:Rule val compose = createAndroidComposeRule<MainActivity>()
    private lateinit var server: TestServer
    private lateinit var runtime: PlaybackRuntime
    private lateinit var store: AppStore
    private lateinit var connection: ServiceConnection

    @Before
    fun setup() {
        server = TestServer()
        val connected = CountDownLatch(1)
        connection =
            object : ServiceConnection {
                override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
                    runtime = (binder as PlaybackService.LocalBinder).playback
                    runtime.mute(true)
                    connected.countDown()
                }

                override fun onServiceDisconnected(name: ComponentName?) {}
            }
        compose.runOnUiThread {
            store = (compose.activity.application as VtampApplication).store
            compose.activity.bindService(
                Intent(compose.activity, PlaybackService::class.java)
                    .setAction(PlaybackService.LOCAL_BIND),
                connection,
                Context.BIND_AUTO_CREATE,
            )
            store.connect(server.base)
        }
        assertTrue(connected.await(5, TimeUnit.SECONDS))
        compose.waitUntil(10000) {
            store.state.value.address == server.base &&
                !store.state.value.connecting &&
                store.state.value.library.size == 3
        }
        compose.runOnUiThread { store.preferVideo(true) }
        compose.onAllNodesWithText("Library").onLast().performClick()
    }

    @After
    fun cleanup() {
        compose.runOnUiThread {
            runtime.clear()
            store.imports.configure(null, false)
            compose.activity.unbindService(connection)
        }
        server.close()
    }

    @Test
    fun libraryPlaybackAndQueueStayLocal() {
        compose.onNodeWithText("Tone").performClick()
        compose.waitUntil(10000) { runtime.state.value.position > 1000 }
        compose.onNodeWithContentDescription("Close player").performClick()
        compose.onNodeWithContentDescription("Actions for Tone").performClick()
        compose.onNodeWithText("Add to Queue").performClick()
        compose.onNodeWithText("Queue").performClick()
        assertEquals(2, runtime.state.value.entries.size)
        assertEquals(2, runtime.state.value.entries.map { it.id }.toSet().size)
        compose.onNodeWithText("Server", useUnmergedTree = true).performClick()
        compose.waitUntil(5000) { store.state.value.serverQueue.size == 2 }
        assertFalse(
            server.requests.any { it in listOf("play", "pause", "next", "queue_add", "queue_edit") }
        )
    }

    @Test
    fun sessionTransportAndBackgroundPlayback() {
        compose.runOnUiThread { runtime.playNow(server.tracks().first()) }
        compose.waitUntil(10000) { runtime.state.value.position > 500 }
        val before = runtime.state.value.position
        @Suppress("DEPRECATION")
        val services =
            (compose.activity.getSystemService(Context.ACTIVITY_SERVICE)
                    as android.app.ActivityManager)
                .getRunningServices(100)
        assertTrue(
            "Playback must own a foreground service",
            services.any { it.service.className.endsWith("PlaybackService") && it.foreground },
        )
        compose.activityRule.scenario.moveToState(Lifecycle.State.CREATED)
        compose.waitUntil(10000) { runtime.state.value.position > before + 1000 }
        compose.runOnUiThread { runtime.sessionPlayer.pause() }
        compose.waitUntil(5000) { !runtime.state.value.playing }
        compose.runOnUiThread { runtime.sessionPlayer.play() }
        compose.waitUntil(10000) {
            runtime.state.value.playing && runtime.state.value.position > before + 2000
        }
        compose.activityRule.scenario.moveToState(Lifecycle.State.RESUMED)
    }

    @Test
    fun importHistoryAndVideoChoiceRefreshLibrary() {
        compose.onNodeWithText("Import").performClick()
        compose.onNodeWithText("YouTube URL").performTextInput("https://youtu.be/fixture0001")
        compose.onAllNodes(isToggleable()).onLast().performClick()
        compose.onNodeWithText("Start Import").performScrollTo().performClick()
        compose.waitUntil(10000) {
            server.submitted != null && store.state.value.library.any { it.id == "imported" }
        }
        assertEquals("true", server.submitted!!["video"].toString())
        compose.onNodeWithTag("importHistory").performScrollToNode(hasText("Load older imports"))
        compose.onNodeWithText("Load older imports").performClick()
        compose.waitUntil(10000) { store.imports.state.value.jobs.size >= 40 }
        assertTrue(store.imports.state.value.jobs.any { it.title == "Import history 066" })
    }

    @Test
    fun videoSuspendsWithoutStoppingAudioAndResumesAtCurrentTime() {
        compose.runOnUiThread { runtime.playNow(server.tracks()[1]) }
        compose.waitUntil(15000) {
            runtime.video.state.value.ready && runtime.state.value.position > 1000
        }
        val before = runtime.state.value.position
        @Suppress("DEPRECATION")
        val services =
            (compose.activity.getSystemService(Context.ACTIVITY_SERVICE)
                    as android.app.ActivityManager)
                .getRunningServices(100)
        assertTrue(
            "Playback must own a foreground service",
            services.any { it.service.className.endsWith("PlaybackService") && it.foreground },
        )
        compose.activityRule.scenario.moveToState(Lifecycle.State.CREATED)
        compose.waitUntil(5000) { runtime.video.player == null }
        val count = server.videoRequests.get()
        compose.waitUntil(10000) { runtime.state.value.position > before + 3000 }
        assertEquals(count, server.videoRequests.get())
        compose.activityRule.scenario.moveToState(Lifecycle.State.RESUMED)
        compose.waitUntil(15000) { runtime.video.state.value.ready }
        compose.runOnUiThread {
            assertTrue(
                kotlin.math.abs(
                    runtime.video.player!!.currentPosition - runtime.audio.currentPosition
                ) < 250
            )
        }
    }

    @Test
    fun radioPauseDisconnectsAndResumeRejoins() {
        compose.runOnUiThread { runtime.playNow(server.tracks()[2]) }
        compose.waitUntil(10000) { runtime.state.value.position > 500 }
        compose.runOnUiThread { runtime.pause() }
        assertFalse(runtime.state.value.playing)
        val count = server.requests.count { it == "GET /radio" }
        compose.runOnUiThread { runtime.play() }
        compose.waitUntil(10000) { server.requests.count { it == "GET /radio" } > count }
        compose.runOnUiThread { runtime.seek(30000) }
        assertTrue(runtime.state.value.position < 30000)
    }
}
