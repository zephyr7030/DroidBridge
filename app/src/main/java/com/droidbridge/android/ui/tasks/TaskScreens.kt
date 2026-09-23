package com.droidbridge.android.ui.tasks

import com.droidbridge.android.product.runtime.PublicResult
import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemColors
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewModelScope
import com.droidbridge.android.R
import com.droidbridge.android.product.tasks.TaskPresentation
import com.droidbridge.android.product.tasks.TaskRepository
import com.droidbridge.android.product.tasks.TaskSnapshot
import com.droidbridge.android.product.tasks.TaskSummary
import com.droidbridge.android.ui.common.RefreshIndicator
import com.droidbridge.android.ui.common.RouteContent
import com.droidbridge.android.ui.common.routeContent
import com.droidbridge.android.ui.common.showsRefresh
import com.droidbridge.android.ui.common.RouteError
import com.droidbridge.android.ui.common.RouteLoading
import java.time.ZoneId
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class TaskDetailUiState(
    val snapshot: TaskSnapshot? = null,
    val loadFailed: Boolean = false,
    val refreshing: Boolean = false,
    val notFound: Boolean = false,
    val mutationFailed: Boolean = false,
)

class TaskDetailViewModel(
    private val repository: TaskRepository,
    private val taskId: String,
) : ViewModel() {
    private val mutableState = MutableStateFlow(TaskDetailUiState())
    val state: StateFlow<TaskDetailUiState> = mutableState.asStateFlow()

    fun refresh() = apply { repository.get(taskId) }

    fun cancel() = apply { repository.cancel(taskId) }

    /** Leaving is consumed once: an exiting entry can compose again and must not pop twice. */
    fun consumeNotFound() = mutableState.update { it.copy(notFound = false) }

    private fun apply(read: suspend () -> PublicResult<TaskSnapshot>) {
        mutableState.update { it.copy(refreshing = true, mutationFailed = false) }
        viewModelScope.launch {
            val result = read()
            mutableState.update { current ->
                when (result) {
                    is PublicResult.Success -> TaskDetailUiState(snapshot = result.value)
                    is PublicResult.Failure -> current.copy(
                        refreshing = false,
                        notFound = result.error.code == NOT_FOUND,
                        loadFailed = current.snapshot == null,
                        mutationFailed = current.snapshot != null,
                    )
                }
            }
        }
    }

    private companion object {
        const val NOT_FOUND = "NOT_FOUND"
    }
}

@Composable
internal fun TaskRow(row: TaskSummary, colors: ListItemColors = ListItemDefaults.colors(), open: () -> Unit) {
    ListItem(
        colors = colors,
        headlineContent = { Text(taskName(row.tool, row.action)) },
        supportingContent = {
            Column {
                Text(stringResource(taskStateLabel(row.state)))
                Text(instant(row.startedAt ?: row.createdAt))
            }
        },
        leadingContent = { Icon(painterResource(taskStateIcon(row.state)), contentDescription = null) },
        modifier = Modifier.clickable(onClick = open).testTag("tasks:row:${row.taskId}"),
    )
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun TaskDetailRoute(viewModel: TaskDetailViewModel, back: () -> Unit) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { viewModel.refresh() }
    // A missing identity returns to the owning list; no placeholder Task is synthesized.
    LaunchedEffect(state.notFound) {
        if (state.notFound) {
            viewModel.consumeNotFound()
            back()
        }
    }
    Scaffold(
        modifier = Modifier.testTag("route:TaskDetail"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.nav_tasks)) },
                navigationIcon = {
                    IconButton(onClick = back, modifier = Modifier.testTag("route:TaskDetail:back")) {
                        Icon(painterResource(R.drawable.ic_arrow_back), stringResource(R.string.nav_tasks))
                    }
                },
                actions = { if (showsRefresh(state.snapshot != null, state.refreshing)) RefreshIndicator("task_detail") },
            )
        },
        bottomBar = {
            if (state.snapshot?.let(TaskPresentation::cancellable) == true) {
                OutlinedButton(
                    onClick = viewModel::cancel,
                    modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp).heightIn(min = 56.dp)
                        .testTag("task_detail:cancel"),
                ) { Text(stringResource(R.string.action_cancel_task)) }
            }
        },
    ) { padding ->
        val snapshot = state.snapshot
        Box(Modifier.fillMaxSize().padding(padding)) {
            when (routeContent(snapshot != null, state.loadFailed || state.notFound)) {
                RouteContent.Error -> RouteError("task_detail", viewModel::refresh)
                RouteContent.Loading -> RouteLoading("task_detail")
                RouteContent.Empty, RouteContent.Content ->
                    TaskDetailContent(requireNotNull(snapshot), state.mutationFailed)
            }
        }
    }
}

@Composable
private fun TaskDetailContent(snapshot: TaskSnapshot, mutationFailed: Boolean) {
    LazyColumn(Modifier.fillMaxSize()) {
        item {
            ListItem(
                headlineContent = { Text(taskName(snapshot.tool, snapshot.action), style = MaterialTheme.typography.titleMedium) },
                supportingContent = {
                    Column {
                        Text(stringResource(taskStateLabel(snapshot.state)))
                        snapshot.executionClass?.let { value ->
                            Text(
                                "${stringResource(R.string.task_detail_execution_class)}  ${executionClassName(value)}",
                                modifier = Modifier.testTag("task_detail:execution_class"),
                            )
                        }
                    }
                },
                leadingContent = { Icon(painterResource(taskStateIcon(snapshot.state)), contentDescription = null) },
                modifier = Modifier.testTag("task_detail:state"),
            )
        }
        item {
            ListItem(
                headlineContent = { TimeLine(R.string.task_detail_created, snapshot.createdAt) },
                supportingContent = if (snapshot.startedAt == null && snapshot.endedAt == null) null else ({
                    Column {
                        snapshot.startedAt?.let { TimeLine(R.string.task_detail_started, it) }
                        snapshot.endedAt?.let { TimeLine(R.string.task_detail_ended, it) }
                    }
                }),
                modifier = Modifier.testTag("task_detail:times"),
            )
        }
        val body = snapshot.result?.let { R.string.task_detail_result to it } ?: snapshot.error?.let { R.string.task_detail_error to it }
        body?.let { (label, value) ->
            item {
                ListItem(
                    headlineContent = { Text(stringResource(label)) },
                    supportingContent = { SelectionContainer { Text(TaskPresentation.prettyJson(value)) } },
                    modifier = Modifier.testTag("task_detail:body"),
                )
            }
        }
        items(TaskPresentation.outputRefs(snapshot.result)) { ref ->
            ListItem(headlineContent = { Text(ref) }, modifier = Modifier.testTag("task_detail:output:$ref"))
        }
        if (mutationFailed) item { RouteError("task_detail:cancel", retry = null) }
    }
}

@Composable
private fun TimeLine(@StringRes label: Int, value: String) {
    Text("${stringResource(label)}  ${instant(value)}")
}

@Composable
private fun instant(value: String): String {
    val locale = LocalConfiguration.current.locales[0]
    return TaskPresentation.formatInstant(value, ZoneId.systemDefault(), locale)
}

/** A Task named in words; an operation this build does not know keeps its wire name. */
@Composable
internal fun taskName(tool: String, action: String): String =
    taskNames["$tool.$action"]?.let { stringResource(it) } ?: "$tool.$action"

private val taskNames = mapOf(
    "context.status" to R.string.task_context_status,
    "context.catalog" to R.string.task_context_catalog,
    "filesystem.inspect" to R.string.task_filesystem_inspect,
    "filesystem.read" to R.string.task_filesystem_read,
    "filesystem.write" to R.string.task_filesystem_write,
    "filesystem.manage" to R.string.task_filesystem_manage,
    "filesystem.download" to R.string.task_filesystem_download,
    "filesystem.archive" to R.string.task_filesystem_archive,
    "command.run" to R.string.task_command_run,
    "network.inspect" to R.string.task_network_inspect,
    "network.capture" to R.string.task_network_capture,
    "network.packet" to R.string.task_network_packet,
    "network.diagnose" to R.string.task_network_diagnose,
    "visual.observe" to R.string.task_visual_observe,
    "visual.view" to R.string.task_visual_view,
    "visual.interact" to R.string.task_visual_interact,
    "android.package" to R.string.task_android_package,
    "android.launch" to R.string.task_android_launch,
    "android.intent" to R.string.task_android_intent,
    "android.clipboard" to R.string.task_android_clipboard,
    "android.notification" to R.string.task_android_notification,
    "automation.execution" to R.string.task_automation_execution,
)

@Composable
private fun executionClassName(value: String): String = when (value) {
    "app" -> stringResource(R.string.execution_class_app)
    "android_framework" -> stringResource(R.string.execution_class_framework)
    "shizuku" -> "Shizuku"
    "magisk" -> "Root"
    else -> value
}

@StringRes
internal fun taskStateLabel(state: String): Int = when (state) {
    "created" -> R.string.task_state_created
    "queued" -> R.string.task_state_queued
    "running" -> R.string.task_state_running
    "completed" -> R.string.task_state_completed
    "failed" -> R.string.task_state_failed
    "cancelled" -> R.string.task_state_cancelled
    "interrupted" -> R.string.task_state_interrupted
    else -> R.string.state_unknown
}

/** The S-UI-014 status icon for a canonical Task state. */
@DrawableRes
internal fun taskStateIcon(state: String): Int = when (state) {
    "completed" -> R.drawable.ic_status_success
    "created", "queued", "running" -> R.drawable.ic_status_schedule
    "failed", "cancelled", "interrupted" -> R.drawable.ic_status_error
    else -> R.drawable.ic_status_unknown
}
