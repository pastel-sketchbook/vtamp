package com.xrath.vtamp.model

import com.xrath.vtamp.api.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.json.*

data class ImportsState(
    val jobs: List<ImportJob> = emptyList(),
    val more: Boolean = false,
    val loading: Boolean = false,
    val error: String? = null,
)

/**
 * Cursor history and live reports have separate lifetimes: polling never collapses revealed pages.
 */
class ImportsStore(private val scope: CoroutineScope, private val libraryChanged: () -> Unit) {
    private val mutable = MutableStateFlow(ImportsState())
    val state = mutable.asStateFlow()
    private var client: VtampClient? = null
    private var paginated = false
    private var generation = 0
    private var cursor: String? = null
    private val seen = linkedMapOf<String, ImportJob>()
    private val visible = linkedSetOf<String>()
    private var legacy = emptyList<ImportJob>()
    private var legacyCount = 20
    private var poll: Job? = null
    private var refreshing = false
    private var loadingMore = false
    private var active = false

    fun configure(client: VtampClient?, pagination: Boolean) {
        if (this.client === client && paginated == pagination) return
        poll?.cancel()
        generation++
        this.client = client
        paginated = pagination
        cursor = null
        seen.clear()
        visible.clear()
        legacy = emptyList()
        legacyCount = 20
        refreshing = false
        loadingMore = false
        mutable.value = ImportsState()
        foreground(active)
    }

    fun foreground(value: Boolean) {
        active = value
        poll?.cancel()
        if (value && client != null)
            poll = scope.launch {
                while (isActive) {
                    refresh()
                    delay(if (seen.values.any { !it.terminal }) 1000 else 10000)
                }
            }
    }

    private fun publish(error: String? = null) {
        mutable.value =
            ImportsState(
                visible.mapNotNull(seen::get).sortedByDescending { it.startedAtMs },
                if (paginated) cursor != null else legacy.size > legacyCount,
                loadingMore,
                error,
            )
    }

    private fun observe(jobs: List<ImportJob>) {
        var changed = false
        for (job in jobs) {
            val old = seen[job.jobId]
            if (old != null && old.revision > job.revision) continue
            if (
                (old != null &&
                    (!old.terminal && job.terminal ||
                        job.added > old.added ||
                        job.updated > old.updated)) ||
                    (old == null && (job.added > 0 || job.updated > 0))
            )
                changed = true
            seen[job.jobId] = job
        }
        if (changed) libraryChanged()
    }

    suspend fun refresh() {
        val c = client ?: return
        if (refreshing) return
        refreshing = true
        val token = generation
        try {
            if (paginated) {
                var page: ImportsPage = c.rpc(command("imports_page") { put("limit", 20) })
                if (token != generation) return
                val known = visible.toSet()
                val fresh = page.jobs.toMutableList()
                val activeJobs = page.activeJobs
                val visited = mutableSetOf<String>()
                // Bridge a burst of new reports to the already revealed history.
                while (
                    known.isNotEmpty() &&
                        fresh.none { it.jobId in known } &&
                        page.nextCursor != null &&
                        fresh.size < 132
                ) {
                    val next = page.nextCursor!!
                    check(visited.add(next)) { "The server repeated an import cursor" }
                    page =
                        c.rpc(
                            command("imports_page") {
                                put("cursor", next)
                                put("limit", 20)
                            }
                        )
                    if (token != generation) return
                    fresh += page.jobs
                }
                val unseenFinished =
                    seen.values.filter {
                        !it.terminal &&
                            activeJobs.none { active -> active.jobId == it.jobId } &&
                            fresh.none { new -> new.jobId == it.jobId }
                    }
                val completed = mutableListOf<ImportJob>()
                for (old in unseenFinished) {
                    try {
                        val report: ImportStatus =
                            c.rpc(
                                command("import_status") {
                                    put("id", old.jobId)
                                    put("offset", 0)
                                    put("limit", 1)
                                }
                            )
                        completed += report.job
                    } catch (e: ApiException) {
                        if (token != generation) return
                        if (e.code !in setOf("import_not_found", "job_not_found", "not_found"))
                            throw e
                        seen.remove(old.jobId)
                        visible.remove(old.jobId)
                        libraryChanged()
                    }
                    if (token != generation) return
                }
                if (known.isEmpty()) cursor = page.nextCursor
                if (known.isNotEmpty() && fresh.none { it.jobId in known }) {
                    visible.clear()
                    cursor = page.nextCursor
                }
                observe(fresh + activeJobs + completed)
                visible += fresh.map { it.jobId }
                // Server retains 100 terminal + 32 active reports.
                val retain =
                    seen.values.filter { !it.terminal }.map { it.jobId }.toSet() +
                        seen.values
                            .filter { it.terminal }
                            .sortedByDescending { it.startedAtMs }
                            .take(100)
                            .map { it.jobId }
                seen.keys.retainAll(retain)
                visible.retainAll(retain)
            } else {
                val jobs: List<ImportJob> = c.rpc(command("imports"))
                if (token != generation) return
                legacy = jobs.sortedByDescending { it.startedAtMs }
                observe(legacy)
                visible.clear()
                visible += legacy.take(legacyCount).map { it.jobId }
                seen.keys.retainAll(legacy.map { it.jobId }.toSet())
            }
            publish()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            if (token == generation) publish(e.message ?: "Cannot load imports")
        } finally {
            if (token == generation) refreshing = false
        }
    }

    fun loadMore() {
        val c = client ?: return
        if (loadingMore) return
        if (!paginated) {
            legacyCount += 20
            visible += legacy.take(legacyCount).map { it.jobId }
            publish()
            return
        }
        val next = cursor ?: return
        val token = generation
        loadingMore = true
        publish()
        scope.launch {
            try {
                val page: ImportsPage =
                    c.rpc(
                        command("imports_page") {
                            put("cursor", next)
                            put("limit", 20)
                        }
                    )
                if (token != generation) return@launch
                check(page.nextCursor != next) { "The server repeated an import cursor" }
                observe(page.jobs + page.activeJobs)
                visible += page.jobs.map { it.jobId }
                cursor = page.nextCursor
                loadingMore = false
                publish()
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                if (token == generation) {
                    loadingMore = false
                    publish(e.message)
                }
            } finally {
                if (token == generation) loadingMore = false
            }
        }
    }
}
