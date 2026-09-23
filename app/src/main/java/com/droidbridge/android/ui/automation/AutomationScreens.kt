package com.droidbridge.android.ui.automation

import com.droidbridge.android.product.runtime.PublicError
import android.content.Intent
import android.content.pm.PackageManager
import androidx.activity.compose.BackHandler
import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDialog
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TimePicker
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberDatePickerState
import androidx.compose.material3.rememberTimePickerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.droidbridge.android.R
import com.droidbridge.android.product.automation.AutomationExecutionRow
import com.droidbridge.android.product.automation.AutomationPlan
import com.droidbridge.android.product.automation.AutomationPlans
import com.droidbridge.android.product.automation.AutomationRow
import com.droidbridge.android.product.automation.AutomationSteps
import com.droidbridge.android.product.automation.ElementBy
import com.droidbridge.android.product.automation.PlanKey
import com.droidbridge.android.product.automation.PlanRunAs
import com.droidbridge.android.product.automation.PlanStep
import com.droidbridge.android.product.automation.PlanTrigger
import com.droidbridge.android.product.automation.StepLine
import java.time.DayOfWeek
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.time.format.TextStyle
import androidx.compose.ui.platform.LocalLocale
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun AutomationsRoute(
    viewModel: AutomationListViewModel,
    openDetail: (String) -> Unit,
    openEditor: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { viewModel.refresh() }
    val snackbar = remember { SnackbarHostState() }
    val bulkResult = state.bulkResult
    val bulkText = bulkResult?.let { stringResource(R.string.snackbar_bulk_delete_result, it.deleted, it.failed) }
    LaunchedEffect(bulkResult) {
        if (bulkText != null) {
            snackbar.showSnackbar(bulkText)
            viewModel.consumeBulkResult()
        }
    }
    val loading = stringResource(R.string.state_loading)
    Scaffold(
        modifier = Modifier.testTag("route:Automations"),
        snackbarHost = { SnackbarHost(snackbar) },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = openEditor,
                icon = { Icon(painterResource(R.drawable.ic_add), contentDescription = null) },
                text = { Text(stringResource(R.string.action_new)) },
                modifier = Modifier.testTag("automations:action_new"),
            )
        },
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.nav_automations)) },
                actions = {
                    if (state.rows != null && (state.refreshing || state.bulkDeleteActive)) {
                        CircularProgressIndicator(
                            modifier = Modifier.size(24.dp).semantics { contentDescription = loading },
                        )
                    }
                    IconButton(
                        onClick = viewModel::requestDeleteAll,
                        enabled = !state.rows.isNullOrEmpty() && !state.bulkDeleteActive,
                        modifier = Modifier.testTag("automations:action_delete_all"),
                    ) {
                        Icon(painterResource(R.drawable.ic_delete), stringResource(R.string.action_delete_all))
                    }
                },
            )
        },
    ) { padding ->
        val rows = state.rows
        Box(Modifier.fillMaxSize().padding(padding)) {
            when {
                rows == null && state.loadFailed -> ErrorItem(retry = viewModel::refresh)
                rows == null -> LoadingIndicator()
                else -> LazyColumn(Modifier.fillMaxSize()) {
                    item(key = "intro") { AutomationIntro(expandedAtFirst = rows.isEmpty()) }
                    if (rows.isEmpty()) {
                        item(key = "empty") {
                            ListItem(
                                headlineContent = { Text(stringResource(R.string.automations_empty)) },
                                leadingContent = { Icon(painterResource(R.drawable.ic_status_unknown), contentDescription = null) },
                                modifier = Modifier.testTag("automations:empty"),
                            )
                        }
                    }
                    items(rows, key = AutomationRow::automationId) { row ->
                        AutomationListItem(row, { openDetail(row.automationId) }) { enabled ->
                            viewModel.setEnabled(row, enabled)
                        }
                    }
                }
            }
        }
    }
    if (state.deleteAllDialog) {
        ConfirmDeleteDialog(
            title = R.string.dialog_delete_all_automations_title,
            body = R.string.dialog_delete_all_automations_body,
            tag = "automations:dialog_delete_all",
            confirm = viewModel::confirmDeleteAll,
            dismiss = viewModel::dismissDeleteAll,
        )
    }
}

/** How an AI builds automations and what they can do; folded to one line once the list has any. */
@Composable
private fun AutomationIntro(expandedAtFirst: Boolean) {
    var expanded by rememberSaveable { mutableStateOf(expandedAtFirst) }
    Card(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)
            .clickable { expanded = !expanded }
            .testTag("automations:intro"),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Icon(painterResource(R.drawable.ic_auto_awesome), contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                Text(stringResource(R.string.automation_intro_title), style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
                Icon(
                    painterResource(if (expanded) R.drawable.ic_arrow_upward else R.drawable.ic_arrow_downward),
                    contentDescription = null,
                    modifier = Modifier.size(20.dp),
                )
            }
            if (expanded) {
                Text(stringResource(R.string.automation_intro_body), style = MaterialTheme.typography.bodyMedium)
                listOf(R.string.automation_intro_when, R.string.automation_intro_what, R.string.automation_intro_ai).forEach {
                    Text(
                        stringResource(it),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun AutomationListItem(row: AutomationRow, open: () -> Unit, setEnabled: (Boolean) -> Unit) {
    val tag = "automations:row:${row.automationId}"
    ListItem(
        headlineContent = { Text(row.name) },
        leadingContent = { Icon(painterResource(triggerIcon(row.trigger)), contentDescription = null) },
        supportingContent = {
            Column {
                row.trigger?.let { Text(triggerText(it)) }
                row.lastExecution?.let { execution -> ExecutionSummary(execution) }
            }
        },
        trailingContent = {
            Switch(checked = row.enabled, onCheckedChange = setEnabled, modifier = Modifier.testTag("$tag:enabled"))
        },
        modifier = Modifier.clickable(onClick = open).testTag(tag),
    )
}

@Composable
private fun ExecutionSummary(execution: AutomationExecutionRow) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        Icon(painterResource(executionIcon(execution.state)), contentDescription = null, modifier = Modifier.size(16.dp))
        Text(executionText(execution))
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun AutomationDetailRoute(
    viewModel: AutomationDetailViewModel,
    edit: () -> Unit,
    openTask: (String) -> Unit,
    back: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(Unit) { viewModel.refresh() }
    // Leaving is consumed once: an exiting entry can compose again and must not pop twice.
    LaunchedEffect(state.deleted, state.loadError) {
        if (state.deleted || state.loadError?.code == NOT_FOUND) {
            viewModel.consumeLeave()
            back()
        }
    }
    val snackbar = remember { SnackbarHostState() }
    val started = stringResource(R.string.snackbar_run_started)
    val failure = state.runError?.let { errorText(it) }
    LaunchedEffect(state.runStarted, failure) {
        when {
            state.runStarted -> snackbar.showSnackbar(started)
            failure != null -> snackbar.showSnackbar(failure)
            else -> return@LaunchedEffect
        }
        viewModel.consumeRunFeedback()
    }
    val detail = state.detail
    Scaffold(
        modifier = Modifier.testTag("route:AutomationDetail"),
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = { Text(detail?.automation?.string("name").orEmpty()) },
                navigationIcon = {
                    IconButton(onClick = back, modifier = Modifier.testTag("route:AutomationDetail:back")) {
                        Icon(painterResource(R.drawable.ic_arrow_back), stringResource(R.string.nav_automations))
                    }
                },
                actions = {
                    if (detail != null) {
                        if (state.plan != null) {
                            IconButton(onClick = edit, modifier = Modifier.testTag("automation_detail:action_edit")) {
                                Icon(painterResource(R.drawable.ic_edit), stringResource(R.string.action_edit))
                            }
                        }
                        IconButton(onClick = viewModel::requestDelete, modifier = Modifier.testTag("automation_detail:action_delete")) {
                            Icon(painterResource(R.drawable.ic_delete), stringResource(R.string.action_delete))
                        }
                    }
                },
            )
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            when {
                detail == null && state.loadError != null -> ErrorItem(retry = viewModel::refresh)
                detail == null -> LoadingIndicator()
                else -> LazyColumn(Modifier.fillMaxSize()) {
                    item(key = "enabled") {
                        ListItem(
                            headlineContent = { Text(triggerText(detail.automation.getValue("trigger") as JsonObject)) },
                            leadingContent = {
                                Icon(painterResource(triggerIcon(detail.automation["trigger"] as? JsonObject)), contentDescription = null)
                            },
                            overlineContent = { Text(stringResource(R.string.automation_trigger)) },
                            trailingContent = {
                                Switch(
                                    checked = detail.enabled,
                                    onCheckedChange = viewModel::setEnabled,
                                    modifier = Modifier.testTag("automation_detail:enabled"),
                                )
                            },
                        )
                    }
                    item(key = "steps") {
                        SectionHeader(R.string.automation_actions)
                        Column(Modifier.padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            AutomationSteps.describe(detail.automation.getValue("action") as JsonObject)
                                .forEachIndexed { index, line -> StepLineText(line, index) }
                            if (state.plan == null) {
                                Row(
                                    verticalAlignment = Alignment.CenterVertically,
                                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                                    modifier = Modifier.padding(top = 4.dp).testTag("automation_detail:readonly"),
                                ) {
                                    Icon(
                                        painterResource(R.drawable.ic_smart_toy),
                                        contentDescription = null,
                                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                                        modifier = Modifier.size(16.dp),
                                    )
                                    Text(
                                        stringResource(R.string.automation_readonly),
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                        }
                        Button(
                            onClick = viewModel::runNow,
                            modifier = Modifier.fillMaxWidth().padding(16.dp).heightIn(min = 48.dp)
                                .testTag("automation_detail:action_run"),
                        ) {
                            Icon(painterResource(R.drawable.ic_play), contentDescription = null)
                            Spacer(Modifier.width(8.dp))
                            Text(stringResource(R.string.action_run_now))
                        }
                        HorizontalDivider()
                    }
                    item(key = "history") { SectionHeader(R.string.automation_history) }
                    if (detail.history.isEmpty()) {
                        item(key = "history:empty") {
                            ListItem(headlineContent = { Text(stringResource(R.string.automation_history_empty)) })
                        }
                    }
                    items(detail.history, key = AutomationExecutionRow::taskId) { execution ->
                        ListItem(
                            headlineContent = { Text(formatInstant(execution.triggeredAt)) },
                            supportingContent = { ExecutionSummary(execution) },
                            modifier = Modifier.clickable { openTask(execution.taskId) }
                                .testTag("automation_detail:run:${execution.taskId}"),
                        )
                    }
                }
            }
        }
    }
    if (state.deleteDialog) {
        ConfirmDeleteDialog(
            title = R.string.dialog_delete_automation_title,
            body = R.string.dialog_delete_automation_body,
            tag = "automation_detail:dialog_delete",
            confirm = viewModel::confirmDelete,
            dismiss = viewModel::dismissDelete,
        )
    }
}

@Composable
private fun StepLineText(line: StepLine, index: Int) {
    val text = when (line) {
        is StepLine.Step -> stepText(line.step)
        is StepLine.Call -> "${line.tool}.${line.action}"
        is StepLine.If -> stringResource(R.string.step_if, conditionText(line))
        is StepLine.Otherwise -> stringResource(R.string.step_otherwise)
        is StepLine.Repeat -> pluralStringResource(R.plurals.step_repeat, line.count, line.count)
        is StepLine.SetState -> stringResource(R.string.step_set_state, line.key, line.value)
    }
    val continues = (line as? StepLine.Step)?.step?.continueOnFailure == true
    val icon = when (line) {
        is StepLine.Step -> line.step.kind().icon
        is StepLine.Call -> R.drawable.ic_extension
        is StepLine.If, is StepLine.Otherwise -> R.drawable.ic_tune
        is StepLine.Repeat -> R.drawable.ic_repeat
        is StepLine.SetState -> R.drawable.ic_tag
    }
    Row(
        Modifier.padding(start = (line.depth * 24).dp).testTag("automation_detail:step:$index"),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(
            painterResource(icon),
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 2.dp).size(20.dp),
        )
        Column {
            Text(text, style = MaterialTheme.typography.bodyLarge)
            if (continues) {
                Text(
                    stringResource(R.string.step_continue),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun conditionText(line: StepLine.If): String = when {
    line.source == "result" && line.key == "succeeded" && line.operator == "equals" && line.value == "true" ->
        stringResource(R.string.condition_last_succeeded)
    line.source == "result" && line.key == "succeeded" && line.operator == "equals" && line.value == "false" ->
        stringResource(R.string.condition_last_failed)
    else -> stringResource(R.string.condition_generic, line.key, line.operator, line.value.orEmpty())
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun AutomationEditorRoute(viewModel: AutomationEditorViewModel, back: () -> Unit) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    // Leaving is consumed once: an exiting entry can compose again and must not pop twice.
    LaunchedEffect(state.finished) {
        if (state.finished) {
            viewModel.consumeFinished()
            back()
        }
    }
    var discardDialog by remember { mutableStateOf(false) }
    val leave = { if (state.dirty) discardDialog = true else back() }
    BackHandler(enabled = state.dirty) { discardDialog = true }
    val plan = state.plan
    Scaffold(
        modifier = Modifier.testTag("route:AutomationEditor"),
        topBar = {
            TopAppBar(
                title = {
                    Text(stringResource(if (state.saved?.name.isNullOrEmpty()) R.string.automation_new_title else R.string.automation_edit_title))
                },
                navigationIcon = {
                    IconButton(onClick = leave, modifier = Modifier.testTag("route:AutomationEditor:back")) {
                        Icon(painterResource(R.drawable.ic_arrow_back), stringResource(R.string.nav_automations))
                    }
                },
            )
        },
        bottomBar = {
            if (plan != null) {
                Column(Modifier.navigationBarsPadding().imePadding().padding(16.dp)) {
                    state.problem?.let { problem ->
                        Text(
                            problemText(problem),
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(bottom = 8.dp).testTag("automation_editor:problem"),
                        )
                    }
                    Button(
                        onClick = viewModel::save,
                        enabled = !state.saving,
                        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("automation_editor:action_save"),
                    ) { Text(stringResource(R.string.action_save)) }
                }
            }
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            when {
                plan == null && state.loadError != null -> ErrorItem(retry = viewModel::retry)
                plan == null && state.saved == null && state.loadError == null && state.plan == null -> LoadingIndicator()
                plan == null -> Text(stringResource(R.string.automation_readonly), Modifier.padding(16.dp))
                else -> PlanForm(plan, viewModel)
            }
        }
    }
    if (discardDialog) {
        AlertDialog(
            onDismissRequest = { discardDialog = false },
            title = { Text(stringResource(R.string.dialog_discard_title)) },
            confirmButton = {
                TextButton(onClick = { discardDialog = false; back() }, modifier = Modifier.testTag("automation_editor:dialog_discard:confirm")) {
                    Text(stringResource(R.string.action_discard))
                }
            },
            dismissButton = {
                TextButton(onClick = { discardDialog = false }) { Text(stringResource(R.string.action_keep_editing)) }
            },
        )
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun PlanForm(plan: AutomationPlan, viewModel: AutomationEditorViewModel) {
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        OutlinedTextField(
            value = plan.name,
            onValueChange = { name -> viewModel.update { it.copy(name = name.take(NAME_LIMIT)) } },
            label = { Text(stringResource(R.string.automation_name)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().testTag("automation_editor:name"),
        )
        SectionTitle(R.string.automation_trigger)
        TriggerForm(plan.trigger) { trigger -> viewModel.update { it.copy(trigger = trigger) } }
        SectionTitle(R.string.automation_actions)
        plan.steps.forEachIndexed { index, step ->
            StepCard(index, step, plan.steps.size, viewModel)
        }
        var chooser by rememberSaveable { mutableStateOf(false) }
        OutlinedButton(
            onClick = { chooser = true },
            modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("automation_editor:add_step"),
        ) {
            Icon(painterResource(R.drawable.ic_add), contentDescription = null)
            Spacer(Modifier.width(8.dp))
            Text(stringResource(R.string.action_add_step))
        }
        if (chooser) {
            ModalBottomSheet(onDismissRequest = { chooser = false }, modifier = Modifier.testTag("automation_editor:add_step:sheet")) {
                StepKind.entries.forEach { kind ->
                    ListItem(
                        headlineContent = { Text(stringResource(kind.label)) },
                        leadingContent = { Icon(painterResource(kind.icon), contentDescription = null) },
                        modifier = Modifier
                            .clickable {
                                chooser = false
                                viewModel.addStep(kind.blank())
                            }
                            .testTag("automation_editor:add_step:${kind.name}"),
                    )
                }
            }
        }
        Spacer(Modifier.heightIn(min = 16.dp))
    }
}

private enum class TriggerKind(@StringRes val label: Int, @DrawableRes val icon: Int) {
    Daily(R.string.trigger_kind_daily, R.drawable.ic_alarm),
    Weekly(R.string.trigger_kind_weekly, R.drawable.ic_date_range),
    Every(R.string.trigger_kind_every, R.drawable.ic_repeat),
    Once(R.string.trigger_kind_once, R.drawable.ic_event),
    Started(R.string.trigger_started, R.drawable.ic_power),
    Network(R.string.trigger_network_any, R.drawable.ic_wifi),
}

@DrawableRes
private fun triggerIcon(trigger: JsonObject?): Int =
    trigger?.let(AutomationPlans::decodeTrigger)?.kind()?.icon ?: R.drawable.ic_tune

private fun PlanTrigger.kind(): TriggerKind = when (this) {
    is PlanTrigger.Daily -> TriggerKind.Daily
    is PlanTrigger.Weekly -> TriggerKind.Weekly
    is PlanTrigger.Every -> TriggerKind.Every
    is PlanTrigger.Once -> TriggerKind.Once
    PlanTrigger.RuntimeStarted -> TriggerKind.Started
    is PlanTrigger.NetworkChanged -> TriggerKind.Network
}

@Composable
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
private fun TriggerForm(trigger: PlanTrigger, change: (PlanTrigger) -> Unit) {
    val time = when (trigger) {
        is PlanTrigger.Daily -> trigger.time
        is PlanTrigger.Weekly -> trigger.time
        is PlanTrigger.Once -> trigger.at.toLocalTime()
        else -> LocalTime.of(8, 0)
    }
    Choice(
        label = "",
        options = TriggerKind.entries,
        selected = trigger.kind(),
        optionLabel = { stringResource(it.label) },
        optionIcon = { it.icon },
        tag = "automation_editor:trigger",
    ) { kind ->
        change(
            when (kind) {
                TriggerKind.Daily -> PlanTrigger.Daily(time)
                TriggerKind.Weekly -> PlanTrigger.Weekly(setOf(LocalDate.now().dayOfWeek), time)
                TriggerKind.Every -> PlanTrigger.Every(60)
                TriggerKind.Once -> PlanTrigger.Once(LocalDate.now().plusDays(1).atTime(time))
                TriggerKind.Started -> PlanTrigger.RuntimeStarted
                TriggerKind.Network -> PlanTrigger.NetworkChanged(null)
            },
        )
    }
    when (trigger) {
        is PlanTrigger.Daily -> TimeField(trigger.time) { change(trigger.copy(time = it)) }
        is PlanTrigger.Weekly -> {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                DayOfWeek.entries.forEach { day ->
                    FilterChip(
                        selected = day in trigger.days,
                        onClick = {
                            change(trigger.copy(days = if (day in trigger.days) trigger.days - day else trigger.days + day))
                        },
                        label = { Text(day.getDisplayName(TextStyle.SHORT, LocalLocale.current.platformLocale)) },
                        modifier = Modifier.testTag("automation_editor:trigger:day:${day.name}"),
                    )
                }
            }
            TimeField(trigger.time) { change(trigger.copy(time = it)) }
        }
        is PlanTrigger.Every -> {
            val hours = trigger.minutes >= 60 && trigger.minutes % 60 == 0L
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                NumberField(
                    value = if (hours) trigger.minutes / 60 else trigger.minutes,
                    label = stringResource(R.string.field_interval),
                    tag = "automation_editor:trigger:every",
                    modifier = Modifier.weight(1f),
                ) { value -> change(PlanTrigger.Every(if (hours) value * 60 else value)) }
                Choice(
                    label = "",
                    options = listOf(false, true),
                    selected = hours,
                    optionLabel = { stringResource(if (it) R.string.unit_hours else R.string.unit_minutes) },
                    tag = "automation_editor:trigger:unit",
                    modifier = Modifier.weight(1f),
                ) { toHours ->
                    val amount = if (hours) trigger.minutes / 60 else trigger.minutes
                    change(PlanTrigger.Every(if (toHours) amount * 60 else amount))
                }
            }
        }
        is PlanTrigger.Once -> {
            DateField(trigger.at.toLocalDate()) { change(trigger.copy(at = it.atTime(trigger.at.toLocalTime()))) }
            TimeField(trigger.at.toLocalTime()) { change(trigger.copy(at = trigger.at.toLocalDate().atTime(it))) }
        }
        is PlanTrigger.NetworkChanged -> Choice(
            label = stringResource(R.string.field_network),
            options = listOf(null) + AutomationPlans.NETWORK_TRANSPORTS.sorted().reversed(),
            selected = trigger.transport,
            optionLabel = { transport ->
                stringResource(
                    when (transport) {
                        "wifi" -> R.string.network_wifi
                        "cellular" -> R.string.network_cellular
                        else -> R.string.network_any
                    },
                )
            },
            tag = "automation_editor:trigger:network",
        ) { change(PlanTrigger.NetworkChanged(it)) }
        PlanTrigger.RuntimeStarted -> {}
    }
}

private enum class StepKind(@StringRes val label: Int, @DrawableRes val icon: Int, val blank: () -> PlanStep) {
    OpenApp(R.string.step_kind_open_app, R.drawable.ic_apps, { PlanStep.OpenApp("") }),
    Tap(R.string.step_kind_tap, R.drawable.ic_touch_app, { PlanStep.TapElement(ElementBy.Text, "") }),
    Type(R.string.step_kind_type, R.drawable.ic_keyboard, { PlanStep.TypeText("") }),
    Key(R.string.step_kind_key, R.drawable.ic_keyboard_return, { PlanStep.PressKey(PlanKey.Back) }),
    Wait(R.string.step_kind_wait, R.drawable.ic_hourglass, { PlanStep.Wait(3) }),
    Command(R.string.step_kind_command, R.drawable.ic_terminal, { PlanStep.RunCommand("") }),
    Copy(R.string.step_kind_copy, R.drawable.ic_content_copy, { PlanStep.CopyText("") }),
}

private fun PlanStep.kind(): StepKind = when (this) {
    is PlanStep.OpenApp -> StepKind.OpenApp
    is PlanStep.TapElement -> StepKind.Tap
    is PlanStep.TypeText -> StepKind.Type
    is PlanStep.PressKey -> StepKind.Key
    is PlanStep.Wait -> StepKind.Wait
    is PlanStep.RunCommand -> StepKind.Command
    is PlanStep.CopyText -> StepKind.Copy
}

@Composable
private fun StepCard(index: Int, step: PlanStep, count: Int, viewModel: AutomationEditorViewModel) {
    val tag = "automation_editor:step:$index"
    val set = { changed: PlanStep -> viewModel.setStep(index, changed) }
    Card(Modifier.fillMaxWidth().testTag(tag)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    painterResource(step.kind().icon),
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(end = 12.dp),
                )
                Text(
                    "${index + 1}. ${stringResource(step.kind().label)}",
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = { viewModel.moveStep(index, -1) }, enabled = index > 0, modifier = Modifier.testTag("$tag:up")) {
                    Icon(painterResource(R.drawable.ic_arrow_upward), stringResource(R.string.action_move_up_step))
                }
                IconButton(onClick = { viewModel.moveStep(index, 1) }, enabled = index < count - 1, modifier = Modifier.testTag("$tag:down")) {
                    Icon(painterResource(R.drawable.ic_arrow_downward), stringResource(R.string.action_move_down_step))
                }
                IconButton(onClick = { viewModel.removeStep(index) }, modifier = Modifier.testTag("$tag:delete")) {
                    Icon(painterResource(R.drawable.ic_delete), stringResource(R.string.action_delete))
                }
            }
            when (step) {
                is PlanStep.OpenApp -> AppField(step.packageName, "$tag:app") { set(step.copy(packageName = it)) }
                is PlanStep.TapElement -> {
                    ElementFields(step.by, step.value, "$tag:element", { set(step.copy(by = it)) }) { set(step.copy(value = it)) }
                    LabeledSwitch(stringResource(R.string.field_long_press), step.longPress, "$tag:long_press") {
                        set(step.copy(longPress = it))
                    }
                    NumberField(step.waitSeconds.toLong(), stringResource(R.string.field_wait_seconds), "$tag:wait") {
                        set(step.copy(waitSeconds = it.toInt()))
                    }
                }
                is PlanStep.TypeText -> {
                    TextField(step.text, stringResource(R.string.field_text), "$tag:text") { set(step.copy(text = it)) }
                    Choice(
                        label = stringResource(R.string.field_type_target),
                        options = listOf(false, true),
                        selected = step.target != null,
                        optionLabel = { stringResource(if (it) R.string.target_element else R.string.target_focused) },
                        tag = "$tag:target_kind",
                    ) { element -> set(step.copy(target = if (element) step.target.orEmpty() else null)) }
                    step.target?.let { target ->
                        TextField(target, stringResource(R.string.field_target_text), "$tag:target") { set(step.copy(target = it)) }
                        NumberField(step.waitSeconds.toLong(), stringResource(R.string.field_wait_seconds), "$tag:wait") {
                            set(step.copy(waitSeconds = it.toInt()))
                        }
                    }
                }
                is PlanStep.PressKey -> Choice(
                    label = stringResource(R.string.field_key),
                    options = PlanKey.entries,
                    selected = step.key,
                    optionLabel = { stringResource(keyLabel(it)) },
                    tag = "$tag:key",
                ) { set(step.copy(key = it)) }
                is PlanStep.Wait -> NumberField(step.seconds.toLong(), stringResource(R.string.field_seconds), "$tag:seconds") {
                    set(step.copy(seconds = it.toInt()))
                }
                is PlanStep.RunCommand -> {
                    TextField(step.command, stringResource(R.string.field_command), "$tag:command", singleLine = false) {
                        set(step.copy(command = it))
                    }
                    Choice(
                        label = stringResource(R.string.field_run_as),
                        options = PlanRunAs.entries,
                        selected = step.runAs,
                        optionLabel = { stringResource(runAsLabel(it)) },
                        tag = "$tag:run_as",
                    ) { set(step.copy(runAs = it)) }
                }
                is PlanStep.CopyText -> TextField(step.text, stringResource(R.string.field_text), "$tag:text") {
                    set(step.copy(text = it))
                }
            }
            if (step !is PlanStep.Wait) {
                LabeledSwitch(stringResource(R.string.field_continue), step.continueOnFailure, "$tag:continue") { continues ->
                    set(
                        when (step) {
                            is PlanStep.OpenApp -> step.copy(continueOnFailure = continues)
                            is PlanStep.TapElement -> step.copy(continueOnFailure = continues)
                            is PlanStep.TypeText -> step.copy(continueOnFailure = continues)
                            is PlanStep.PressKey -> step.copy(continueOnFailure = continues)
                            is PlanStep.RunCommand -> step.copy(continueOnFailure = continues)
                            is PlanStep.CopyText -> step.copy(continueOnFailure = continues)
                            is PlanStep.Wait -> step
                        },
                    )
                }
            }
        }
    }
}

@Composable
private fun ElementFields(
    by: ElementBy,
    value: String,
    tag: String,
    changeBy: (ElementBy) -> Unit,
    changeValue: (String) -> Unit,
) {
    TextField(value, stringResource(R.string.field_element_value), "$tag:value", onChange = changeValue)
    Choice(
        label = stringResource(R.string.field_find_by),
        options = ElementBy.entries,
        selected = by,
        optionLabel = { stringResource(byLabel(it)) },
        tag = "$tag:by",
        onSelect = changeBy,
    )
}

/** A launchable app, labelled as the launcher shows it. */
private data class LaunchableApp(val packageName: String, val label: String)

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun AppField(packageName: String, tag: String, change: (String) -> Unit) {
    val context = LocalContext.current
    var picking by rememberSaveable { mutableStateOf(false) }
    OutlinedButton(
        onClick = { picking = true },
        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag(tag),
    ) {
        Text(if (packageName.isEmpty()) stringResource(R.string.pick_app) else appLabel(context.packageManager, packageName))
    }
    if (picking) {
        val locale = LocalLocale.current.platformLocale
        val apps = remember {
            context.packageManager
                .queryIntentActivities(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER), 0)
                .map { LaunchableApp(it.activityInfo.packageName, it.loadLabel(context.packageManager).toString()) }
                .distinctBy { it.packageName }
                .filter { it.packageName != context.packageName }
                .sortedBy { it.label.lowercase(locale) }
        }
        ModalBottomSheet(onDismissRequest = { picking = false }, modifier = Modifier.testTag("$tag:sheet")) {
            LazyColumn {
                items(apps, key = LaunchableApp::packageName) { app ->
                    ListItem(
                        headlineContent = { Text(app.label) },
                        supportingContent = { Text(app.packageName) },
                        modifier = Modifier
                            .clickable {
                                picking = false
                                change(app.packageName)
                            }
                            .testTag("$tag:sheet:${app.packageName}"),
                    )
                }
            }
        }
    }
}

private fun appLabel(packageManager: PackageManager, packageName: String): String = try {
    packageManager.getApplicationInfo(packageName, 0).loadLabel(packageManager).toString()
} catch (_: PackageManager.NameNotFoundException) {
    packageName
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun TimeField(time: LocalTime, change: (LocalTime) -> Unit) {
    var open by rememberSaveable { mutableStateOf(false) }
    OutlinedButton(
        onClick = { open = true },
        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("automation_editor:trigger:time"),
    ) { Text("${stringResource(R.string.field_time)}  ${time.format(TIME)}") }
    if (open) {
        val picker = rememberTimePickerState(time.hour, time.minute, is24Hour = true)
        AlertDialog(
            onDismissRequest = { open = false },
            text = { TimePicker(state = picker) },
            confirmButton = {
                TextButton(onClick = {
                    open = false
                    change(LocalTime.of(picker.hour, picker.minute))
                }, modifier = Modifier.testTag("automation_editor:trigger:time:confirm")) { Text(stringResource(android.R.string.ok)) }
            },
            dismissButton = { TextButton(onClick = { open = false }) { Text(stringResource(R.string.action_cancel)) } },
        )
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun DateField(date: LocalDate, change: (LocalDate) -> Unit) {
    var open by rememberSaveable { mutableStateOf(false) }
    OutlinedButton(
        onClick = { open = true },
        modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("automation_editor:trigger:date"),
    ) { Text("${stringResource(R.string.field_date)}  ${date.format(DateTimeFormatter.ofLocalizedDate(FormatStyle.MEDIUM))}") }
    if (open) {
        // The picker counts UTC midnights, so the date is converted without a zone shift.
        val picker = rememberDatePickerState(initialSelectedDateMillis = date.atStartOfDay(ZoneOffset.UTC).toInstant().toEpochMilli())
        DatePickerDialog(
            onDismissRequest = { open = false },
            confirmButton = {
                TextButton(onClick = {
                    open = false
                    picker.selectedDateMillis?.let { change(Instant.ofEpochMilli(it).atZone(ZoneOffset.UTC).toLocalDate()) }
                }) { Text(stringResource(android.R.string.ok)) }
            },
            dismissButton = { TextButton(onClick = { open = false }) { Text(stringResource(R.string.action_cancel)) } },
        ) { DatePicker(state = picker) }
    }
}

@Composable
private fun TextField(
    value: String,
    label: String,
    tag: String,
    singleLine: Boolean = true,
    onChange: (String) -> Unit,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onChange,
        label = { Text(label) },
        singleLine = singleLine,
        modifier = Modifier.fillMaxWidth().testTag(tag),
    )
}

/** A whole-number field; text that is not a number reports -1, which the editor rejects. */
@Composable
private fun NumberField(
    value: Long,
    label: String,
    tag: String,
    modifier: Modifier = Modifier,
    onChange: (Long) -> Unit,
) {
    var text by remember(tag) { mutableStateOf(value.toString()) }
    OutlinedTextField(
        value = text,
        onValueChange = { typed ->
            text = typed.filter(Char::isDigit).take(NUMBER_DIGITS)
            onChange(text.toLongOrNull() ?: -1)
        },
        label = { Text(label) },
        singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
        modifier = modifier.fillMaxWidth().testTag(tag),
    )
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun <T> Choice(
    label: String,
    options: List<T>,
    selected: T,
    optionLabel: @Composable (T) -> String,
    tag: String,
    modifier: Modifier = Modifier,
    optionIcon: ((T) -> Int)? = null,
    onSelect: (T) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = it }, modifier = modifier.fillMaxWidth()) {
        OutlinedTextField(
            value = optionLabel(selected),
            onValueChange = {},
            readOnly = true,
            label = if (label.isEmpty()) null else ({ Text(label) }),
            leadingIcon = optionIcon?.let { icon -> { Icon(painterResource(icon(selected)), contentDescription = null) } },
            singleLine = true,
            modifier = Modifier.fillMaxWidth()
                .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable)
                .testTag(tag),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEachIndexed { index, option ->
                DropdownMenuItem(
                    text = { Text(optionLabel(option)) },
                    leadingIcon = optionIcon?.let { icon -> { Icon(painterResource(icon(option)), contentDescription = null) } },
                    onClick = {
                        expanded = false
                        onSelect(option)
                    },
                    modifier = Modifier.testTag("$tag:option:$index"),
                )
            }
        }
    }
}

@Composable
private fun LabeledSwitch(label: String, checked: Boolean, tag: String, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Text(label, modifier = Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = onChange, modifier = Modifier.testTag(tag))
    }
}

@Composable
private fun ConfirmDeleteDialog(
    @StringRes title: Int,
    @StringRes body: Int,
    tag: String,
    confirm: () -> Unit,
    dismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(stringResource(title)) },
        text = { Text(stringResource(body)) },
        confirmButton = {
            TextButton(onClick = confirm, modifier = Modifier.testTag("$tag:confirm")) { Text(stringResource(R.string.action_delete)) }
        },
        dismissButton = {
            TextButton(onClick = dismiss, modifier = Modifier.testTag("$tag:cancel")) { Text(stringResource(R.string.action_cancel)) }
        },
    )
}

@Composable
private fun SectionTitle(@StringRes title: Int) {
    Text(stringResource(title), style = MaterialTheme.typography.titleMedium)
}

@Composable
private fun SectionHeader(@StringRes title: Int) {
    Text(
        stringResource(title),
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
    )
}

@Composable
private fun LoadingIndicator() {
    val loading = stringResource(R.string.state_loading)
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(modifier = Modifier.semantics { contentDescription = loading })
    }
}

@Composable
private fun ErrorItem(retry: () -> Unit) {
    ListItem(
        headlineContent = { Text(stringResource(R.string.state_error)) },
        leadingContent = { Icon(painterResource(R.drawable.ic_status_error), contentDescription = null) },
        trailingContent = {
            TextButton(onClick = retry, modifier = Modifier.testTag("automations:action_retry")) {
                Text(stringResource(R.string.action_retry))
            }
        },
    )
}

@Composable
private fun triggerText(trigger: JsonObject): String {
    val locale = LocalLocale.current.platformLocale
    return triggerText(trigger, locale)
}

@Composable
private fun triggerText(trigger: JsonObject, locale: java.util.Locale): String = when (val plan = AutomationPlans.decodeTrigger(trigger)) {
    is PlanTrigger.Daily -> stringResource(R.string.trigger_daily, plan.time.format(TIME))
    is PlanTrigger.Weekly -> stringResource(
        R.string.trigger_weekly,
        DayOfWeek.entries.filter { it in plan.days }.joinToString("、") { it.getDisplayName(TextStyle.SHORT, locale) },
        plan.time.format(TIME),
    )
    is PlanTrigger.Every -> if (plan.minutes % 60 == 0L) {
        stringResource(R.string.trigger_every_hours, plan.minutes / 60)
    } else {
        stringResource(R.string.trigger_every_minutes, plan.minutes)
    }
    is PlanTrigger.Once -> stringResource(
        R.string.trigger_once,
        plan.at.format(DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT)),
    )
    PlanTrigger.RuntimeStarted -> stringResource(R.string.trigger_started)
    is PlanTrigger.NetworkChanged -> stringResource(
        when (plan.transport) {
            "wifi" -> R.string.trigger_network_wifi
            "cellular" -> R.string.trigger_network_cellular
            else -> R.string.trigger_network_any
        },
    )
    null -> stringResource(R.string.trigger_custom)
}

@Composable
private fun stepText(step: PlanStep): String = when (step) {
    is PlanStep.OpenApp -> stringResource(R.string.step_open_app, appLabel(LocalContext.current.packageManager, step.packageName))
    is PlanStep.TapElement -> stringResource(if (step.longPress) R.string.step_long_press else R.string.step_tap, step.value)
    is PlanStep.TypeText -> step.target?.let { stringResource(R.string.step_type_into, it, step.text) }
        ?: stringResource(R.string.step_type_focused, step.text)
    is PlanStep.PressKey -> stringResource(
        when (step.key) {
            PlanKey.Back -> R.string.step_key_back
            PlanKey.Home -> R.string.step_key_home
            PlanKey.Recents -> R.string.step_key_recents
            PlanKey.Enter -> R.string.step_key_enter
        },
    )
    is PlanStep.Wait -> stringResource(R.string.step_wait, step.seconds)
    is PlanStep.RunCommand -> stringResource(R.string.step_command, step.command)
    is PlanStep.CopyText -> stringResource(R.string.step_copy, step.text)
}

@Composable
private fun executionText(execution: AutomationExecutionRow): String {
    val state = executionLabel(execution.state)?.let { stringResource(it) } ?: execution.state
    val error = execution.errorCode?.let { runErrorLabel(it)?.let { label -> stringResource(label) } ?: it }
    return listOfNotNull(state, error).joinToString(" · ")
}

@Composable
private fun errorText(error: PublicError): String =
    runErrorLabel(error.code)?.let { stringResource(it) } ?: error.message ?: error.code

@Composable
private fun problemText(problem: PlanProblem): String = when (problem) {
    PlanProblem.NameRequired -> stringResource(R.string.error_name_required)
    PlanProblem.StepsRequired -> stringResource(R.string.error_steps_required)
    is PlanProblem.StepIncomplete -> stringResource(R.string.error_step_incomplete, problem.number)
    PlanProblem.OncePast -> stringResource(R.string.error_once_past)
    PlanProblem.WeeklyDays -> stringResource(R.string.error_weekly_days)
    PlanProblem.Interval -> stringResource(R.string.error_interval)
    is PlanProblem.SaveFailed -> stringResource(R.string.error_save_failed, errorText(problem.error))
}

@StringRes
private fun runErrorLabel(code: String): Int? = when (code) {
    "NOT_FOUND" -> R.string.run_error_not_found
    "EXECUTION_FAILED" -> R.string.run_error_failed
    "TIMEOUT" -> R.string.run_error_timeout
    "CAPABILITY_UNAVAILABLE" -> R.string.run_error_unavailable
    "RUN_AS_UNAVAILABLE" -> R.string.run_error_run_as
    "PERMISSION_DENIED" -> R.string.run_error_permission
    else -> null
}

@StringRes
private fun byLabel(by: ElementBy): Int = when (by) {
    ElementBy.Text -> R.string.by_text
    ElementBy.TextContains -> R.string.by_text_contains
    ElementBy.Description -> R.string.by_description
    ElementBy.ResourceId -> R.string.by_resource_id
}

@StringRes
private fun keyLabel(key: PlanKey): Int = when (key) {
    PlanKey.Back -> R.string.key_back
    PlanKey.Home -> R.string.key_home
    PlanKey.Recents -> R.string.key_recents
    PlanKey.Enter -> R.string.key_enter
}

@StringRes
private fun runAsLabel(runAs: PlanRunAs): Int = when (runAs) {
    PlanRunAs.App -> R.string.run_as_app
    PlanRunAs.Shell -> R.string.run_as_shell
    PlanRunAs.Root -> R.string.run_as_root
}

@StringRes
private fun executionLabel(state: String): Int? = when (state) {
    "created" -> R.string.task_state_created
    "queued" -> R.string.task_state_queued
    "running" -> R.string.task_state_running
    "completed" -> R.string.task_state_completed
    "failed" -> R.string.task_state_failed
    "cancelled" -> R.string.task_state_cancelled
    "interrupted" -> R.string.task_state_interrupted
    else -> null
}

@DrawableRes
private fun executionIcon(state: String): Int = when (state) {
    "completed" -> R.drawable.ic_status_success
    "created", "queued", "running" -> R.drawable.ic_status_schedule
    "failed", "cancelled", "interrupted" -> R.drawable.ic_status_error
    else -> R.drawable.ic_status_unknown
}

private fun formatInstant(value: String): String = try {
    OffsetDateTime.parse(value)
        .atZoneSameInstant(ZoneId.systemDefault())
        .format(DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM, FormatStyle.SHORT))
} catch (_: RuntimeException) {
    value
}

private fun JsonObject.string(key: String): String? = (this[key] as? JsonPrimitive)?.contentOrNull

private val TIME: DateTimeFormatter = DateTimeFormatter.ofPattern("HH:mm")
private const val NAME_LIMIT = 128
private const val NUMBER_DIGITS = 6
private const val NOT_FOUND = "NOT_FOUND"
