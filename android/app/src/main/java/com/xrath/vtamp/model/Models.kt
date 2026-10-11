package com.xrath.vtamp.model

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class Track(
    val id: String,
    val title: String,
    val artist: String = "",
    val album: String = "",
    val path: String? = null,
    val url: String? = null,
    val cover: String? = null,
    val video: Boolean = false,
    @SerialName("duration_ms") val durationMs: Long? = null,
    @SerialName("track_number") val trackNumber: Int = 0,
) {
    val live
        get() = url != null

    val subtitle
        get() =
            listOf(artist.takeUnless { it == "Unknown artist" }.orEmpty(), album)
                .filter { it.isNotBlank() }
                .joinToString(" · ")
                .ifEmpty {
                    if (live) runCatching { java.net.URI(url!!).host }.getOrNull().orEmpty() else ""
                }
}

@Serializable
data class ServerDescriptor(
    val mode: String,
    val version: String,
    @SerialName("protocol_version") val protocolVersion: Int,
    @SerialName("import_available") val importAvailable: Boolean = false,
    @SerialName("imports_pagination") val importsPagination: Boolean = false,
)

@Serializable data class LibraryPage(val tracks: List<Track>, val total: Int, val offset: Int)

@Serializable data class ServerQueueItem(val id: String, val track: Track)

@Serializable
data class QueuePage(
    val items: List<ServerQueueItem>,
    val total: Int,
    val offset: Int,
    @SerialName("queue_revision") val queueRevision: Long,
)

@Serializable
data class ServerNow(
    val current: ServerQueueItem? = null,
    val status: String,
    @SerialName("position_ms") val positionMs: Long = 0,
    @SerialName("queue_length") val queueLength: Int = 0,
    @SerialName("queue_revision") val queueRevision: Long = 0,
)

@Serializable
data class TimeRange(
    @SerialName("start_ms") val startMs: Long,
    @SerialName("end_ms") val endMs: Long? = null,
)

@Serializable
data class ImportRequest(
    val url: String,
    val playlist: Boolean = false,
    val video: Boolean = false,
    val range: TimeRange? = null,
)

@Serializable data class ImportStarted(@SerialName("job_id") val jobId: String, val status: String)

@Serializable
data class ImportProgress(
    val bytes: Long? = null,
    val total: Long? = null,
    @SerialName("processed_ms") val processedMs: Long? = null,
    @SerialName("processing_total_ms") val processingTotalMs: Long? = null,
) {
    val fraction: Float?
        get() {
            val progress = if ((processingTotalMs ?: 0) > 0) processedMs else bytes
            val length = if ((processingTotalMs ?: 0) > 0) processingTotalMs else total
            return if (progress != null && length != null && length > 0)
                (progress.toFloat() / length).coerceIn(0f, 1f)
            else null
        }
}

@Serializable
data class ImportJob(
    @SerialName("job_id") val jobId: String,
    val url: String,
    val title: String = "",
    val status: String,
    val stage: String = "",
    val added: Int = 0,
    val updated: Int = 0,
    val failed: Int = 0,
    val skipped: Int = 0,
    @SerialName("video_failed") val videoFailed: Int = 0,
    @SerialName("first_added_track_id") val firstAddedTrackId: String? = null,
    @SerialName("current_title") val currentTitle: String? = null,
    val progress: ImportProgress = ImportProgress(),
    @SerialName("started_at_ms") val startedAtMs: Long = 0,
    val error: String? = null,
    val revision: Long = 0,
) {
    val terminal
        get() = status in setOf("completed", "partial", "failed", "cancelled", "interrupted")

    val retryable
        get() = status in setOf("partial", "failed", "cancelled", "interrupted")
}

@Serializable
data class ImportsPage(
    val jobs: List<ImportJob>,
    @SerialName("next_cursor") val nextCursor: String? = null,
    @SerialName("active_jobs") val activeJobs: List<ImportJob> = emptyList(),
)

@Serializable data class ImportStatus(val job: ImportJob)

fun timeText(ms: Long?): String {
    if (ms == null) return "LIVE"
    val seconds = ms.coerceAtLeast(0) / 1000
    return if (seconds >= 3600)
        "%d:%02d:%02d"
            .format(java.util.Locale.ROOT, seconds / 3600, seconds / 60 % 60, seconds % 60)
    else "%d:%02d".format(java.util.Locale.ROOT, seconds / 60, seconds % 60)
}

fun parseTime(text: String): Long? {
    val parts = text.trim().split(':')
    if (parts.size !in 1..3 || parts.any { it.isEmpty() || it.any { c -> c !in '0'..'9' } })
        return null
    val numbers = parts.map { it.toLongOrNull() ?: return null }
    if (numbers.drop(1).any { it >= 60 } || parts.drop(1).any { it.length != 2 }) return null
    return runCatching {
            numbers
                .fold(0L) { value, next -> Math.addExact(Math.multiplyExact(value, 60), next) }
                .let { Math.multiplyExact(it, 1000) }
        }
        .getOrNull()
        ?.takeIf { it <= Long.MAX_VALUE / 1000 }
}

fun parseRange(start: String, end: String): TimeRange? {
    if (start.isBlank() && end.isBlank()) return null
    val from =
        if (start.isBlank()) 0
        else requireNotNull(parseTime(start)) { "Use seconds, M:SS, or H:MM:SS for Start" }
    val to =
        if (end.isBlank()) null
        else requireNotNull(parseTime(end)) { "Use seconds, M:SS, or H:MM:SS for End" }
    require(to == null || to > from) { "End must be after Start" }
    return if (from == 0L && to == null) null else TimeRange(from, to)
}
