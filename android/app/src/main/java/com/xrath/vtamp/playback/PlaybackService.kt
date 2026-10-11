@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package com.xrath.vtamp.playback

import android.app.PendingIntent
import android.content.Intent
import android.os.Binder
import android.os.IBinder
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService
import com.xrath.vtamp.MainActivity
import com.xrath.vtamp.VtampApplication

class PlaybackService : MediaSessionService() {
    lateinit var runtime: PlaybackRuntime
        private set

    private var session: MediaSession? = null

    inner class LocalBinder : Binder() {
        val playback
            get() = runtime
    }

    override fun onCreate() {
        super.onCreate()
        val app = application as VtampApplication
        runtime = PlaybackRuntime(this, app.store)
        session =
            MediaSession.Builder(this, runtime.sessionPlayer)
                .setSessionActivity(
                    PendingIntent.getActivity(
                        this,
                        0,
                        Intent(this, MainActivity::class.java),
                        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                    )
                )
                .build()
        addSession(session!!)
        app.store.onServerChanged = runtime::clear
    }

    override fun onBind(intent: Intent?): IBinder? =
        if (intent?.action == LOCAL_BIND) LocalBinder() else super.onBind(intent)

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? = session

    override fun onTaskRemoved(rootIntent: Intent?) {
        if (!runtime.state.value.playing) stopSelf()
    }

    override fun onDestroy() {
        (application as VtampApplication).store.onServerChanged = null
        session?.release()
        runtime.close()
        super.onDestroy()
    }

    companion object {
        const val LOCAL_BIND = "com.xrath.vtamp.LOCAL_BIND"
    }
}
