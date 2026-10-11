@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package com.xrath.vtamp.playback

import android.content.Context
import android.graphics.Bitmap
import android.view.TextureView
import androidx.media3.common.*
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.SeekParameters
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.ui.PlayerView
import com.xrath.vtamp.AppStore
import java.util.concurrent.TimeUnit
import kotlin.math.abs
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import okhttp3.OkHttpClient

data class VideoState(
    val aspect: Float = 1f,
    val ready: Boolean = false,
    val error: String? = null,
    val still: Bitmap? = null,
)

/** Video may chase audio; it can never pause, seek, or change the speed of audio. */
class VideoFollower(
    private val context: Context,
    private val playback: PlaybackRuntime,
    private val store: AppStore,
) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val mutable = MutableStateFlow(VideoState())
    val state = mutable.asStateFlow()
    var player: ExoPlayer? = null
        private set

    private var view: PlayerView? = null
    private var sync: Job? = null
    private var generation = 0
    private var frameTimeMs = Long.MIN_VALUE
    private var currentId: String? = null
    private var lastSeekAt = 0L

    fun show(surface: PlayerView) {
        view = surface
        val s = playback.state.value
        val track = s.current ?: return
        val client = store.state.value.client ?: return
        if (!track.video || !store.state.value.video) return
        if (currentId != s.currentId) reset()
        currentId = s.currentId
        view = surface
        if (player != null) {
            surface.player = player
            return
        }
        val token = ++generation
        mutable.update { it.copy(ready = false, error = null) }
        frameTimeMs = Long.MIN_VALUE
        val video =
            ExoPlayer.Builder(context)
                .setMediaSourceFactory(
                    DefaultMediaSourceFactory(
                        OkHttpDataSource.Factory(
                            OkHttpClient.Builder()
                                .connectTimeout(10, TimeUnit.SECONDS)
                                .readTimeout(15, TimeUnit.SECONDS)
                                .build()
                        )
                    )
                )
                .build()
        player = video
        video.trackSelectionParameters =
            video.trackSelectionParameters
                .buildUpon()
                .setTrackTypeDisabled(C.TRACK_TYPE_AUDIO, true)
                .build()
        video.volume = 0f
        video.setSeekParameters(SeekParameters.EXACT)
        video.setVideoFrameMetadataListener { presentationTimeUs, _, _, _ ->
            scope.launch { if (generation == token) frameTimeMs = presentationTimeUs / 1000 }
        }
        video.addListener(
            object : Player.Listener {
                override fun onVideoSizeChanged(videoSize: VideoSize) {
                    if (token == generation && videoSize.height > 0)
                        mutable.update {
                            it.copy(
                                aspect =
                                    videoSize.width * videoSize.pixelWidthHeightRatio /
                                        videoSize.height
                            )
                        }
                }

                override fun onTracksChanged(tracks: Tracks) {
                    if (
                        token == generation &&
                            tracks.containsType(C.TRACK_TYPE_VIDEO) &&
                            !tracks.isTypeSupported(C.TRACK_TYPE_VIDEO)
                    )
                        failed("This device cannot decode this video", token)
                }

                override fun onPlayerError(error: PlaybackException) {
                    failed("Video unavailable on this device. Audio continues.", token)
                }
            }
        )
        surface.player = video
        video.setMediaItem(
            MediaItem.fromUri(client.file(track, "video")),
            playback.audio.currentPosition,
        )
        video.prepare()
        video.playWhenReady = playback.audio.isPlaying
        sync = scope.launch {
            while (isActive && generation == token) {
                val audio = playback.audio
                val target = audio.currentPosition
                video.playWhenReady = audio.isPlaying
                val delta = video.currentPosition - target
                val now = android.os.SystemClock.elapsedRealtime()
                if (abs(delta) > 250 && now - lastSeekAt > 1000) {
                    holdFrame()
                    video.seekTo(target)
                    frameTimeMs = Long.MIN_VALUE
                    lastSeekAt = now
                } else
                    video.setPlaybackSpeed(
                        if (delta > 70) 0.97f else if (delta < -70) 1.03f else 1f
                    )
                if (frameTimeMs != Long.MIN_VALUE && abs(frameTimeMs - target) <= 150)
                    mutable.update { it.copy(ready = true) }
                delay(100)
            }
        }
    }

    private fun holdFrame() {
        val image = (view?.videoSurfaceView as? TextureView)?.bitmap
        mutable.update { it.copy(ready = false, still = image ?: it.still) }
    }

    private fun failed(message: String, token: Int) {
        if (generation != token) return
        hide()
        mutable.update { it.copy(error = message, still = null) }
    }

    fun seek() {
        val p = player ?: return
        holdFrame()
        frameTimeMs = Long.MIN_VALUE
        p.seekTo(playback.audio.currentPosition)
        lastSeekAt = android.os.SystemClock.elapsedRealtime()
    }

    fun hide() {
        if (player != null) holdFrame()
        generation++
        sync?.cancel()
        sync = null
        view?.player = null
        view = null
        player?.release()
        player = null
    }

    fun reset() {
        hide()
        currentId = null
        mutable.value = VideoState()
    }

    fun close() {
        reset()
        scope.cancel()
    }
}
