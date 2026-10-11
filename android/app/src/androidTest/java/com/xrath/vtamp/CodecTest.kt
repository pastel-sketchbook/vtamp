@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package com.xrath.vtamp

import android.graphics.SurfaceTexture
import android.net.Uri
import android.view.Surface
import androidx.media3.common.*
import androidx.media3.decoder.ffmpeg.FfmpegLibrary
import androidx.media3.exoplayer.*
import androidx.media3.exoplayer.analytics.AnalyticsListener
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class CodecTest {
    @Test
    fun aacAlacAndWavReallyDecode() {
        assertTrue("ALAC must be packaged", FfmpegLibrary.isAvailable())
        listOf("stereo-aac.m4a", "extended-mdat.m4a", "stereo-alac.m4a", "stereo.wav").forEach {
            decode(it, false)
        }
    }

    @Test
    fun h264AndVp9MatroskaDecodeFrames() {
        decode("video-h264.mkv", true)
        decode("video-vp9.mkv", true)
    }

    private fun decode(name: String, video: Boolean) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val file =
            File(context.cacheDir, name).apply {
                writeBytes(instrumentation.context.assets.open(name).use { it.readBytes() })
            }
        val finished = CountDownLatch(1)
        var error: PlaybackException? = null
        var decoder = ""
        var frames = 0
        lateinit var player: ExoPlayer
        var texture: SurfaceTexture? = null
        var surface: Surface? = null
        instrumentation.runOnMainSync {
            player =
                ExoPlayer.Builder(
                        context,
                        DefaultRenderersFactory(context)
                            .setExtensionRendererMode(
                                DefaultRenderersFactory.EXTENSION_RENDERER_MODE_ON
                            ),
                    )
                    .build()
            player.volume = 0f
            if (video) {
                texture = SurfaceTexture(0).apply { setDefaultBufferSize(64, 64) }
                surface = Surface(texture)
                player.setVideoSurface(surface)
                player.setVideoFrameMetadataListener { _, _, _, _ -> frames++ }
            }
            player.addAnalyticsListener(
                object : AnalyticsListener {
                    override fun onAudioDecoderInitialized(
                        eventTime: AnalyticsListener.EventTime,
                        decoderName: String,
                        initializedTimestampMs: Long,
                        initializationDurationMs: Long,
                    ) {
                        decoder = decoderName
                    }
                }
            )
            player.addListener(
                object : Player.Listener {
                    override fun onPlaybackStateChanged(playbackState: Int) {
                        if (playbackState == Player.STATE_ENDED) finished.countDown()
                    }

                    override fun onPlayerError(failure: PlaybackException) {
                        error = failure
                        finished.countDown()
                    }
                }
            )
            player.setMediaItem(MediaItem.fromUri(Uri.fromFile(file)))
            player.prepare()
            player.play()
        }
        try {
            assertTrue("$name timed out", finished.await(12, TimeUnit.SECONDS))
            assertNull("$name: ${error?.message}", error)
            if (video) assertTrue("No video frames decoded", frames > 0)
            if (name == "stereo-alac.m4a")
                assertTrue("Expected FFmpeg ALAC, got $decoder", decoder.contains("alac", true))
        } finally {
            instrumentation.runOnMainSync {
                player.release()
                surface?.release()
                texture?.release()
            }
            file.delete()
        }
    }
}
