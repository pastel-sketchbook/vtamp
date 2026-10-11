@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package com.xrath.vtamp.playback

import android.content.Context
import android.graphics.Bitmap
import android.os.Handler
import android.os.Looper
import android.view.PixelCopy
import android.view.SurfaceView
import androidx.media3.common.*
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.DefaultLoadControl
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
    private var captureGeneration = 0

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
                .setLoadControl(
                    DefaultLoadControl.Builder()
                        .setBufferDurationsMs(15000, 50000, 250, 500)
                        .build()
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
                val delta = video.currentPosition - target
                if (
                    !audio.isPlaying &&
                        audio.playbackState == Player.STATE_READY &&
                        video.playbackState == Player.STATE_READY &&
                        abs(delta) > 150
                ) {
                    // Audio is stationary, so a single seek can settle its paused frame.
                    seek()
                }
                // Startup/rebuffering can leave video behind. Repeated exact seeks
                // flush the decoder before it can catch up, especially on phones.
                // Let preparation finish, then catch up without interrupting decode.
                // If video is ahead, wait for audio instead of seeking backwards.
                video.playWhenReady = audio.isPlaying && delta <= 150
                val speed =
                    if (video.playbackState != Player.STATE_READY || !audio.isPlaying) 1f
                    else if (delta < -1000) 1.5f
                    else if (delta < -250) 1.25f
                    else if (delta < -70) 1.05f else if (delta > 70) 0.97f else 1f
                if (video.playbackParameters.speed != speed) video.setPlaybackSpeed(speed)
                if (frameTimeMs != Long.MIN_VALUE && abs(frameTimeMs - target) <= 150)
                    mutable.update { it.copy(ready = true) }
                delay(100)
            }
        }
    }

    private fun holdFrame() {
        val hadFrame = mutable.value.ready
        mutable.update { it.copy(ready = false) }
        val token = ++captureGeneration
        val id = currentId
        val surface = view?.videoSurfaceView as? SurfaceView ?: return
        if (
            !hadFrame ||
                !surface.holder.surface.isValid ||
                surface.width <= 0 ||
                surface.height <= 0
        )
            return
        val image = Bitmap.createBitmap(surface.width, surface.height, Bitmap.Config.ARGB_8888)
        try {
            // SurfaceView preserves HDR output. Copy asynchronously instead of reading a
            // TextureView bitmap, and never let an old track/seek replace a newer frame.
            PixelCopy.request(
                surface,
                image,
                { result ->
                    if (
                        result == PixelCopy.SUCCESS &&
                            token == captureGeneration &&
                            id == currentId &&
                            !mutable.value.ready
                    ) {
                        mutable.update { it.copy(still = image) }
                    } else image.recycle()
                },
                Handler(Looper.getMainLooper()),
            )
        } catch (_: IllegalArgumentException) {
            // The surface may be destroyed while the activity is being backgrounded.
            image.recycle()
        }
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
        p.setPlaybackSpeed(1f)
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
        captureGeneration++
        currentId = null
        mutable.value = VideoState()
    }

    fun close() {
        reset()
        scope.cancel()
    }
}
