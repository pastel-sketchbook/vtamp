package com.xrath.vtamp.playback

import java.io.IOException
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import okhttp3.*

internal data class RadioSource(val uri: String, val mimeType: String?)

internal class RadioHttpException(val status: Int) : IOException("Radio answered HTTP $status")

/** Media3 selects HLS before opening a URL, so extensionless station redirects need resolving. */
internal suspend fun resolveRadio(http: OkHttpClient, uri: String): RadioSource =
    suspendCancellableCoroutine { continuation ->
        // Use GET: station gateways do not necessarily support HEAD. Close without reading the
        // broadcast body. The resolved (possibly signed) URL lives only for this playback attempt.
        val call = http.newCall(Request.Builder().url(uri).build())
        continuation.invokeOnCancellation { call.cancel() }
        call.enqueue(
            object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    if (!continuation.isCancelled) continuation.resumeWithException(e)
                }

                override fun onResponse(call: Call, response: Response) {
                    response.use {
                        if (continuation.isCancelled) return
                        if (!it.isSuccessful) {
                            continuation.resumeWithException(RadioHttpException(it.code))
                            return
                        }
                        val type =
                            it.header("Content-Type")?.substringBefore(';')?.trim()?.lowercase()
                        val hls =
                            type in
                                setOf(
                                    "application/vnd.apple.mpegurl",
                                    "application/x-mpegurl",
                                    "audio/mpegurl",
                                    "audio/x-mpegurl",
                                ) || it.request.url.encodedPath.endsWith(".m3u8", ignoreCase = true)
                        continuation.resume(
                            RadioSource(
                                it.request.url.toString(),
                                if (hls) "application/x-mpegURL" else null,
                            )
                        )
                    }
                }
            }
        )
    }
