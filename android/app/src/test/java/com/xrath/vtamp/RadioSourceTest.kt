package com.xrath.vtamp

import com.xrath.vtamp.playback.*
import kotlinx.coroutines.runBlocking
import okhttp3.OkHttpClient
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Test

class RadioSourceTest {
    @Test
    fun resolvesHlsFromRedirectTargetOrResponseType() = runBlocking {
        MockWebServer().use { server ->
            server.start()
            val http = OkHttpClient()
            for ((target, type) in
                listOf(
                    "/live.m3u8?token=temporary" to "application/octet-stream",
                    "/live" to "Application/Vnd.Apple.MpegURL; charset=UTF-8",
                    "/live" to "audio/x-mpegurl",
                )) {
                server.enqueue(MockResponse().setResponseCode(302).setHeader("Location", target))
                server.enqueue(MockResponse().setHeader("Content-Type", type).setBody("#EXTM3U\n"))
                val source = resolveRadio(http, server.url("/station").toString())
                assertEquals(server.url(target).toString(), source.uri)
                assertEquals("application/x-mpegURL", source.mimeType)
            }
        }
    }

    @Test
    fun progressiveRadioKeepsAutomaticExtractorSelection() = runBlocking {
        MockWebServer().use { server ->
            server.start()
            server.enqueue(MockResponse().setHeader("Content-Type", "audio/mpeg").setBody("audio"))
            val source = resolveRadio(OkHttpClient(), server.url("/station").toString())
            assertNull(source.mimeType)
        }
    }

    @Test
    fun httpFailuresKeepStatusForRetryPolicy() = runBlocking {
        MockWebServer().use { server ->
            server.start()
            for (status in listOf(404, 503)) {
                server.enqueue(MockResponse().setResponseCode(status))
                try {
                    resolveRadio(OkHttpClient(), server.url("/station").toString())
                    fail("Expected HTTP failure")
                } catch (error: RadioHttpException) {
                    assertEquals(status, error.status)
                }
            }
        }
    }
}
