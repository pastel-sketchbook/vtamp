package com.xrath.vtamp.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.xrath.vtamp.*
import com.xrath.vtamp.api.ApiException
import com.xrath.vtamp.model.*
import com.xrath.vtamp.playback.PlaybackRuntime
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ImportScreen(app: AppState, store: AppStore, playback: PlaybackRuntime?) {
    val imports by store.imports.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    var url by rememberSaveable(app.address) { mutableStateOf("") }
    var start by rememberSaveable(app.address) { mutableStateOf("") }
    var end by rememberSaveable(app.address) { mutableStateOf("") }
    var playlist by rememberSaveable(app.address) { mutableStateOf(false) }
    var video by rememberSaveable(app.address) { mutableStateOf(false) }
    var busy by remember(app.address) { mutableStateOf(false) }
    var error by remember(app.address) { mutableStateOf<String?>(null) }
    var uncertain by rememberSaveable(app.address) { mutableStateOf(false) }
    var actionBusy by remember(app.address) { mutableStateOf<String?>(null) }
    Column {
        TopAppBar(
            title = { Text("Import") },
            actions = {
                IconButton(onClick = { scope.launch { store.imports.refresh() } }) {
                    Icon(Icons.Default.Refresh, "Refresh imports")
                }
            },
        )
        LazyColumn(
            Modifier.fillMaxSize().testTag("importHistory"),
            contentPadding = PaddingValues(bottom = 16.dp),
        ) {
            item {
                Column(
                    Modifier.padding(horizontal = 16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Text("Save to your server", style = MaterialTheme.typography.titleLarge)
                    Text(
                        "Import a YouTube video or playlist. Downloads run on your server.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    OutlinedTextField(
                        url,
                        { url = it },
                        Modifier.fillMaxWidth(),
                        label = { Text("YouTube URL") },
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                    )
                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        OutlinedTextField(
                            start,
                            { start = it },
                            Modifier.weight(1f),
                            label = { Text("Start (optional)") },
                            placeholder = { Text("1:23") },
                            singleLine = true,
                        )
                        OutlinedTextField(
                            end,
                            { end = it },
                            Modifier.weight(1f),
                            label = { Text("End (optional)") },
                            placeholder = { Text("2:45") },
                            singleLine = true,
                        )
                    }
                    Text("Seconds, M:SS, or H:MM:SS", style = MaterialTheme.typography.bodySmall)
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        Text("Whole playlist", Modifier.weight(1f))
                        Switch(
                            playlist,
                            { playlist = it },
                            Modifier.semantics { contentDescription = "Whole playlist" },
                        )
                    }
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        Text("Save video (up to 480p)", Modifier.weight(1f))
                        Switch(
                            video,
                            { video = it },
                            Modifier.semantics { contentDescription = "Save video" },
                        )
                    }
                    error?.let { ErrorMessage(it) }
                    if (uncertain) {
                        Text(
                            "The submission may have reached the server. Check recent imports before submitting again.",
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        TextButton(
                            onClick = {
                                scope.launch {
                                    store.imports.refresh()
                                    uncertain = false
                                }
                            }
                        ) {
                            Text("Refresh recent imports")
                        }
                    }
                    Button(
                        onClick = {
                            val request =
                                try {
                                    require(
                                        url.trim().startsWith("https://") ||
                                            url.trim().startsWith("http://")
                                    ) {
                                        "Enter a YouTube HTTP(S) URL"
                                    }
                                    ImportRequest(
                                        url.trim(),
                                        playlist,
                                        video,
                                        parseRange(start, end),
                                    )
                                } catch (e: IllegalArgumentException) {
                                    error = e.message
                                    return@Button
                                }
                            busy = true
                            error = null
                            uncertain = true
                            val source = app.client
                            scope.launch {
                                try {
                                    store.submit(request)
                                    if (store.state.value.client === source) {
                                        url = ""
                                        start = ""
                                        end = ""
                                        playlist = false
                                        video = false
                                        uncertain = false
                                    }
                                } catch (e: CancellationException) {
                                    throw e
                                } catch (e: Exception) {
                                    if (store.state.value.client === source) {
                                        error = e.message ?: "Import failed"
                                        uncertain =
                                            e !is ApiException ||
                                                e.code in setOf("malformed", "http")
                                    }
                                } finally {
                                    busy = false
                                }
                            }
                        },
                        enabled = url.isNotBlank() && !busy && !uncertain,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        if (busy) {
                            CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                            Spacer(Modifier.width(8.dp))
                        }
                        Text(if (busy) "Starting…" else "Start Import")
                    }
                    HorizontalDivider(Modifier.padding(vertical = 12.dp))
                    Text("Recent imports", style = MaterialTheme.typography.titleLarge)
                }
            }
            imports.error?.let { message ->
                item { ErrorMessage(message, { scope.launch { store.imports.refresh() } }) }
            }
            if (imports.jobs.isEmpty())
                item {
                    EmptyState(
                        "No imports yet",
                        "Paste a YouTube URL above to add it to your Library.",
                        Icons.Default.Download,
                    )
                }
            items(imports.jobs, key = { it.jobId }) { job ->
                Column(
                    Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    Text(
                        job.title.ifBlank { job.url },
                        style = MaterialTheme.typography.titleMedium,
                    )
                    Text(
                        listOf(
                                job.status.replaceFirstChar(Char::uppercase),
                                job.stage.replace('_', ' '),
                            )
                            .filter { it.isNotBlank() }
                            .joinToString(" · "),
                        style = MaterialTheme.typography.bodySmall,
                    )
                    job.currentTitle?.let {
                        Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 2)
                    }
                    if (!job.terminal)
                        job.progress.fraction?.let { fraction ->
                            LinearProgressIndicator(
                                progress = { fraction },
                                modifier = Modifier.fillMaxWidth(),
                            )
                        } ?: LinearProgressIndicator(Modifier.fillMaxWidth())
                    if (job.terminal)
                        Text(
                            "${job.added} added · ${job.updated} updated · ${job.failed} failed",
                            style = MaterialTheme.typography.bodySmall,
                        )
                    job.error?.let {
                        Text(
                            it,
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        if (!job.terminal || job.retryable)
                            TextButton(
                                enabled = actionBusy == null,
                                onClick = {
                                    actionBusy = job.jobId
                                    scope.launch {
                                        try {
                                            store.importAction(job, job.retryable)
                                        } catch (e: CancellationException) {
                                            throw e
                                        } catch (e: Exception) {
                                            error = e.message
                                        } finally {
                                            actionBusy = null
                                        }
                                    }
                                },
                            ) {
                                Text(if (job.retryable) "Retry" else "Cancel")
                            }
                        job.firstAddedTrackId?.let { id ->
                            TextButton(
                                onClick = {
                                    val client = app.client ?: return@TextButton
                                    scope.launch {
                                        try {
                                            val track = client.track(id)
                                            if (store.state.value.client === client)
                                                playback?.playNow(track)
                                        } catch (e: CancellationException) {
                                            throw e
                                        } catch (e: Exception) {
                                            error = e.message
                                        }
                                    }
                                }
                            ) {
                                Text("Play")
                            }
                        }
                    }
                    HorizontalDivider()
                }
            }
            if (imports.more)
                item {
                    TextButton(
                        onClick = store.imports::loadMore,
                        enabled = !imports.loading,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(if (imports.loading) "Loading…" else "Load older imports")
                    }
                }
        }
    }
}
