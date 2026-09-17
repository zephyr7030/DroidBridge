package com.droidbridge.android.ui.automation

import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.ExposedDropdownMenuAnchorType
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.droidbridge.android.R
import com.droidbridge.android.product.automation.ACTION_TYPE
import com.droidbridge.android.product.automation.ActionDraft
import com.droidbridge.android.product.automation.AutomationDescriptorCatalog
import com.droidbridge.android.product.automation.AutomationRow
import com.droidbridge.android.product.automation.DescriptorControl
import com.droidbridge.android.product.automation.DescriptorOption
import com.droidbridge.android.product.automation.DescriptorSection
import com.droidbridge.android.product.automation.DescriptorValueKind
import com.droidbridge.android.product.automation.FieldDescriptor
import com.droidbridge.android.product.automation.editorText
import com.droidbridge.android.product.automation.integerValue
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun AutomationsRoute(
    viewModel: AutomationListViewModel,
    openEditor: (String?) -> Unit,
    openTask: (String) -> Unit,
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
                onClick = { openEditor(null) },
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
                rows.isEmpty() -> ListItem(
                    headlineContent = { Text(stringResource(R.string.automations_empty)) },
                    leadingContent = { Icon(painterResource(R.drawable.ic_status_unknown), contentDescription = null) },
                    modifier = Modifier.testTag("automations:empty"),
                )
                else -> LazyColumn(Modifier.fillMaxSize()) {
                    items(rows, key = AutomationRow::automationId) { row ->
                        AutomationListItem(row, openEditor, openTask) { enabled -> viewModel.setEnabled(row, enabled) }
                    }
                }
            }
        }
    }
    if (state.deleteAllDialog) {
        AlertDialog(
            onDismissRequest = viewModel::dismissDeleteAll,
            title = { Text(stringResource(R.string.dialog_delete_all_automations_title)) },
            text = { Text(stringResource(R.string.dialog_delete_all_automations_body)) },
            confirmButton = {
                TextButton(onClick = viewModel::confirmDeleteAll, modifier = Modifier.testTag("automations:dialog_delete_all:confirm")) {
                    Text(stringResource(R.string.action_delete))
                }
            },
            dismissButton = {
                TextButton(onClick = viewModel::dismissDeleteAll, modifier = Modifier.testTag("automations:dialog_delete_all:cancel")) {
                    Text(stringResource(R.string.action_cancel))
                }
            },
        )
    }
}

@Composable
private fun AutomationListItem(
    row: AutomationRow,
    openEditor: (String?) -> Unit,
    openTask: (String) -> Unit,
    setEnabled: (Boolean) -> Unit,
) {
    val tag = "automations:row:${row.automationId}"
    ListItem(
        headlineContent = { Text(row.name) },
        supportingContent = {
            Column {
                row.triggerType?.let { type -> triggerLabel(type)?.let { Text(stringResource(it)) } }
                row.lastExecution?.let { execution ->
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        Icon(painterResource(executionIcon(execution.state)), contentDescription = null, modifier = Modifier.size(16.dp))
                        executionLabel(execution.state)?.let { Text(stringResource(it)) }
                    }
                }
            }
        },
        trailingContent = {
            Row(verticalAlignment = Alignment.CenterVertically) {
                row.lastExecution?.let { execution ->
                    TextButton(onClick = { openTask(execution.taskId) }, modifier = Modifier.testTag("$tag:action_view_task")) {
                        Text(stringResource(R.string.action_view_task))
                    }
                }
                Switch(checked = row.enabled, onCheckedChange = setEnabled, modifier = Modifier.testTag("$tag:enabled"))
            }
        },
        modifier = Modifier.clickable { openEditor(row.automationId) }.testTag(tag),
    )
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun AutomationEditorRoute(
    viewModel: AutomationEditorViewModel,
    catalog: AutomationDescriptorCatalog,
    onBack: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.finished) { if (state.finished) onBack() }
    // A missing identity returns to the owning list after its NOT_FOUND presentation (S-UI-014).
    LaunchedEffect(state.loadError) { if (state.loadError?.code == NOT_FOUND) onBack() }
    val draft = state.draft
    Scaffold(
        modifier = Modifier.testTag("route:AutomationEditor"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.nav_automations)) },
                navigationIcon = {
                    IconButton(onClick = onBack, modifier = Modifier.testTag("route:AutomationEditor:back")) {
                        Icon(painterResource(R.drawable.ic_arrow_back), stringResource(R.string.nav_automations))
                    }
                },
                actions = {
                    if (draft?.automationId != null) {
                        IconButton(onClick = viewModel::requestDelete, modifier = Modifier.testTag("automation_editor:action_delete")) {
                            Icon(painterResource(R.drawable.ic_delete), stringResource(R.string.action_delete))
                        }
                    }
                },
            )
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            when {
                draft == null && state.loadError != null -> ErrorItem(retry = viewModel::retry)
                draft == null -> LoadingIndicator()
                else -> Column(
                    modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    val root = draft.values
                    catalog.visible(DescriptorSection.Root, root).forEach { field ->
                        DescriptorField(field, root[field.canonicalPath], ROOT_TAG) { viewModel.setRootValue(field.canonicalPath, it) }
                    }
                    SectionTitle(R.string.automation_trigger)
                    catalog.visible(DescriptorSection.Trigger, root).forEach { field ->
                        DescriptorField(field, root[field.canonicalPath], ROOT_TAG) { viewModel.setRootValue(field.canonicalPath, it) }
                    }
                    SectionTitle(R.string.automation_actions)
                    ActionNodeCard(catalog, root, draft.action, emptyList(), viewModel)
                    val keys = referencedStateKeys(draft)
                    if (keys.isNotEmpty()) {
                        SectionTitle(R.string.automation_state_keys)
                        keys.forEach { key -> Text(key, modifier = Modifier.testTag("automation_editor:state_key:$key")) }
                    }
                    state.validationError?.let { error ->
                        SectionTitle(R.string.automation_validation_errors)
                        // The Contract result is displayed verbatim; the UI copies no bound or message.
                        Text(error.code, modifier = Modifier.testTag("automation_editor:validation_code"))
                        error.message?.let { Text(it) }
                    }
                    Button(
                        onClick = viewModel::save,
                        enabled = !state.saving,
                        modifier = Modifier.fillMaxWidth().testTag("automation_editor:action_save"),
                    ) { Text(stringResource(R.string.action_save)) }
                }
            }
        }
    }
    if (state.deleteDialog) {
        AlertDialog(
            onDismissRequest = viewModel::dismissDelete,
            title = { Text(stringResource(R.string.dialog_delete_automation_title)) },
            text = { Text(stringResource(R.string.dialog_delete_automation_body)) },
            confirmButton = {
                TextButton(onClick = viewModel::confirmDelete, modifier = Modifier.testTag("automation_editor:dialog_delete:confirm")) {
                    Text(stringResource(R.string.action_delete))
                }
            },
            dismissButton = {
                TextButton(onClick = viewModel::dismissDelete, modifier = Modifier.testTag("automation_editor:dialog_delete:cancel")) {
                    Text(stringResource(R.string.action_cancel))
                }
            },
        )
    }
}

@Composable
private fun ActionNodeCard(
    catalog: AutomationDescriptorCatalog,
    root: Map<String, JsonElement>,
    node: ActionDraft,
    ref: List<NodeStep>,
    viewModel: AutomationEditorViewModel,
    controls: (@Composable RowScope.() -> Unit)? = null,
) {
    val tag = "automation_editor:node:${ref.tagPath()}"
    Card(modifier = Modifier.fillMaxWidth().testTag(tag)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            controls?.let { Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End, content = it) }
            val context = root + node.values
            listOf(DescriptorSection.Action, DescriptorSection.CallArguments).forEach { section ->
                catalog.visible(section, context).forEach { field ->
                    DescriptorField(field, node.values[field.canonicalPath], tag) { value ->
                        if (field.canonicalPath == ACTION_TYPE) {
                            (value as? JsonPrimitive)?.contentOrNull?.let { viewModel.changeNodeType(ref, it) }
                        } else {
                            viewModel.setNodeValue(ref, field.canonicalPath, value)
                        }
                    }
                }
            }
            when (node.type) {
                "sequence" -> {
                    node.children.forEachIndexed { index, child ->
                        ActionNodeCard(catalog, root, child, ref + NodeStep.Child(index), viewModel) {
                            val childTag = "automation_editor:node:${(ref + NodeStep.Child(index)).tagPath()}"
                            IconButton(
                                onClick = { viewModel.moveChild(ref, index, -1) },
                                enabled = index > 0,
                                modifier = Modifier.testTag("$childTag:action_move_up"),
                            ) { Icon(painterResource(R.drawable.ic_arrow_upward), stringResource(R.string.action_move_up)) }
                            IconButton(
                                onClick = { viewModel.moveChild(ref, index, 1) },
                                enabled = index < node.children.lastIndex,
                                modifier = Modifier.testTag("$childTag:action_move_down"),
                            ) { Icon(painterResource(R.drawable.ic_arrow_downward), stringResource(R.string.action_move_down)) }
                            IconButton(
                                onClick = { viewModel.removeChild(ref, index) },
                                modifier = Modifier.testTag("$childTag:action_delete"),
                            ) { Icon(painterResource(R.drawable.ic_delete), stringResource(R.string.action_delete)) }
                        }
                    }
                    AddActionButton(catalog, "$tag:add_child") { type -> viewModel.addChild(ref, type) }
                }
                "conditional" -> {
                    ActionSlot(catalog, root, node.thenAction, ref, NodeStep.Then, removable = false, viewModel)
                    ActionSlot(catalog, root, node.elseAction, ref, NodeStep.Else, removable = true, viewModel)
                }
                "repeat" -> ActionSlot(catalog, root, node.repeatedAction, ref, NodeStep.Repeated, removable = false, viewModel)
            }
        }
    }
}

@Composable
private fun ActionSlot(
    catalog: AutomationDescriptorCatalog,
    root: Map<String, JsonElement>,
    child: ActionDraft?,
    ref: List<NodeStep>,
    slot: NodeStep,
    removable: Boolean,
    viewModel: AutomationEditorViewModel,
) {
    val slotRef = ref + slot
    if (child == null) {
        AddActionButton(catalog, "automation_editor:node:${slotRef.tagPath()}:add") { type ->
            viewModel.setSlot(ref, slot, type)
        }
        return
    }
    ActionNodeCard(
        catalog,
        root,
        child,
        slotRef,
        viewModel,
        controls = if (removable) {
            {
                IconButton(
                    onClick = { viewModel.setSlot(ref, slot, null) },
                    modifier = Modifier.testTag("automation_editor:node:${slotRef.tagPath()}:action_delete"),
                ) { Icon(painterResource(R.drawable.ic_delete), stringResource(R.string.action_delete)) }
            }
        } else {
            null
        },
    )
}

/** Opens the S-UI-008 action chooser whose choices are the descriptor's own `/action/type` options. */
@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun AddActionButton(catalog: AutomationDescriptorCatalog, tag: String, add: (String) -> Unit) {
    var open by rememberSaveable(tag) { mutableStateOf(false) }
    IconButton(onClick = { open = true }, modifier = Modifier.testTag(tag)) {
        Icon(painterResource(R.drawable.ic_add), stringResource(R.string.action_new))
    }
    if (open) {
        val options = catalog.fields.first { it.canonicalPath == ACTION_TYPE }.options
        ModalBottomSheet(onDismissRequest = { open = false }, modifier = Modifier.testTag("$tag:sheet")) {
            options.forEach { option ->
                ListItem(
                    headlineContent = { Text(optionLabel(option)) },
                    modifier = Modifier
                        .clickable {
                            open = false
                            add(option.value)
                        }
                        .testTag("$tag:sheet:${option.value}"),
                )
            }
        }
    }
}

/** Renders exactly one descriptor control; no field presentation is inferred from names or types. */
@Composable
private fun DescriptorField(
    field: FieldDescriptor,
    value: JsonElement?,
    tagPrefix: String,
    onChange: (JsonElement?) -> Unit,
) {
    val label = fieldLabel(field)
    val tag = "$tagPrefix:field:${field.canonicalPath}"
    when (field.control) {
        DescriptorControl.Switch -> LabeledSwitch(label, (value as? JsonPrimitive)?.booleanOrNull == true, tag) {
            onChange(JsonPrimitive(it))
        }
        DescriptorControl.OutlinedText -> TextControl(field.valueKind, label, value, tag, onChange)
        DescriptorControl.SingleChoice -> ChoiceControl(field.options, label, value, tag, onChange)
        DescriptorControl.Optional -> Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            // Starts excluded; including it begins with the wrapped control's empty input.
            LabeledSwitch(label, value != null, "$tag:include") { include ->
                onChange(
                    if (!include) {
                        null
                    } else if (field.wrappedControl == DescriptorControl.SingleChoice) {
                        JsonPrimitive(field.options.first().value)
                    } else {
                        JsonPrimitive("")
                    },
                )
            }
            if (value != null) {
                if (field.wrappedControl == DescriptorControl.SingleChoice) {
                    ChoiceControl(field.options, label, value, tag, onChange)
                } else {
                    TextControl(field.valueKind, label, value, tag, onChange)
                }
            }
        }
        DescriptorControl.ScalarEditor -> ScalarControl(label, value, tag, onChange)
        DescriptorControl.KeyScalarTable -> TableControl(field, label, value.asObjectOrEmpty(), tag) { table ->
            onChange(table.takeIf { it.isNotEmpty() })
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
private fun TextControl(
    kind: DescriptorValueKind,
    label: String,
    value: JsonElement?,
    tag: String,
    onChange: (JsonElement) -> Unit,
) {
    OutlinedTextField(
        value = value.editorText(),
        onValueChange = { text -> onChange(if (kind == DescriptorValueKind.Integer) integerValue(text) else JsonPrimitive(text)) },
        label = { Text(label) },
        modifier = Modifier.fillMaxWidth().testTag(tag),
    )
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun ChoiceControl(
    options: List<DescriptorOption>,
    label: String,
    value: JsonElement?,
    tag: String,
    onChange: (JsonElement) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val selected = options.firstOrNull { it.value == (value as? JsonPrimitive)?.contentOrNull }
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = it }) {
        OutlinedTextField(
            value = selected?.let { optionLabel(it) }.orEmpty(),
            onValueChange = {},
            readOnly = true,
            label = { Text(label) },
            modifier = Modifier
                .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable)
                .fillMaxWidth()
                .testTag(tag),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { option ->
                DropdownMenuItem(
                    text = { Text(optionLabel(option)) },
                    onClick = {
                        expanded = false
                        onChange(JsonPrimitive(option.value))
                    },
                    modifier = Modifier.testTag("$tag:option:${option.value}"),
                )
            }
        }
    }
}

/** The explicit redundant editor for one primitive wire value (S-UI-008 `scalar_editor`). */
@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun ScalarControl(label: String, value: JsonElement?, tag: String, onChange: (JsonElement) -> Unit) {
    val loadedKind = when {
        value == null -> null
        value is JsonNull -> ScalarKind.Null
        value is JsonPrimitive && value.isString -> ScalarKind.String
        value is JsonPrimitive && value.booleanOrNull != null -> ScalarKind.Boolean
        value is JsonPrimitive -> ScalarKind.Integer
        else -> null
    }
    var kind by rememberSaveable(tag) { mutableStateOf(loadedKind) }
    var expanded by remember { mutableStateOf(false) }
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = it }) {
            OutlinedTextField(
                value = kind?.let { stringResource(it.label) }.orEmpty(),
                onValueChange = {},
                readOnly = true,
                label = { Text(label) },
                modifier = Modifier
                    .menuAnchor(ExposedDropdownMenuAnchorType.PrimaryNotEditable)
                    .fillMaxWidth()
                    .testTag("$tag:type"),
            )
            ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                ScalarKind.entries.forEach { choice ->
                    DropdownMenuItem(
                        text = { Text(stringResource(choice.label)) },
                        onClick = {
                            expanded = false
                            if (choice != kind) {
                                // Changing type discards the inactive draft input.
                                kind = choice
                                onChange(
                                    when (choice) {
                                        ScalarKind.Null -> JsonNull
                                        ScalarKind.Boolean -> JsonPrimitive(false)
                                        ScalarKind.Integer -> integerValue("")
                                        ScalarKind.String -> JsonPrimitive("")
                                    },
                                )
                            }
                        },
                        modifier = Modifier.testTag("$tag:type:${choice.wire}"),
                    )
                }
            }
        }
        when (kind) {
            null, ScalarKind.Null -> Unit
            ScalarKind.Boolean -> LabeledSwitch(
                stringResource(R.string.scalar_type_boolean),
                (value as? JsonPrimitive)?.booleanOrNull == true,
                "$tag:boolean",
            ) { onChange(JsonPrimitive(it)) }
            ScalarKind.Integer -> OutlinedTextField(
                value = value.editorText(),
                onValueChange = { onChange(integerValue(it)) },
                label = { Text(stringResource(R.string.scalar_type_integer)) },
                modifier = Modifier.fillMaxWidth().testTag("$tag:integer"),
            )
            ScalarKind.String -> OutlinedTextField(
                value = value.editorText(),
                onValueChange = { onChange(JsonPrimitive(it)) },
                label = { Text(stringResource(R.string.scalar_type_string)) },
                modifier = Modifier.fillMaxWidth().testTag("$tag:string"),
            )
        }
    }
}

/** The event `match` table: at most `max_rows` key/scalar rows (S-UI-008 `key_scalar_table`). */
@Composable
private fun TableControl(
    field: FieldDescriptor,
    label: String,
    table: JsonObject,
    tag: String,
    onChange: (JsonObject) -> Unit,
) {
    val rows = table.entries.toList()
    Column(Modifier.fillMaxWidth().testTag(tag), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(label, style = MaterialTheme.typography.bodyLarge)
        rows.forEachIndexed { index, (key, scalar) ->
            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(8.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        OutlinedTextField(
                            value = key,
                            onValueChange = { renamed ->
                                onChange(JsonObject(rows.mapIndexed { position, (k, v) -> (if (position == index) renamed else k) to v }.toMap()))
                            },
                            label = { Text(label) },
                            modifier = Modifier.weight(1f).testTag("$tag:row:$index:key"),
                        )
                        IconButton(
                            onClick = { onChange(JsonObject(table - key)) },
                            modifier = Modifier.testTag("$tag:row:$index:action_delete"),
                        ) { Icon(painterResource(R.drawable.ic_delete), stringResource(R.string.action_delete)) }
                    }
                    ScalarControl(label, scalar, "$tag:row:$index:value") { updated ->
                        onChange(JsonObject(table + (key to updated)))
                    }
                }
            }
        }
        if (rows.size < (field.maxRows ?: 0) && "" !in table) {
            IconButton(
                onClick = { onChange(JsonObject(table + ("" to JsonNull))) },
                modifier = Modifier.testTag("$tag:action_new"),
            ) { Icon(painterResource(R.drawable.ic_add), stringResource(R.string.action_new)) }
        }
    }
}

@Composable
private fun SectionTitle(@StringRes title: Int) {
    Text(stringResource(title), style = MaterialTheme.typography.titleMedium)
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
private fun fieldLabel(field: FieldDescriptor): String =
    field.labelResource?.let { stringResource(descriptorString(it)) } ?: field.wireName

@Composable
private fun optionLabel(option: DescriptorOption): String =
    option.labelResource?.let { stringResource(descriptorString(it)) } ?: option.value

private enum class ScalarKind(val wire: String, @StringRes val label: Int) {
    Null("null", R.string.scalar_type_null),
    Boolean("boolean", R.string.scalar_type_boolean),
    Integer("integer", R.string.scalar_type_integer),
    String("string", R.string.scalar_type_string),
}

/** The closed S-UI-008 structural label set; any other resource name is a descriptor defect. */
@StringRes
private fun descriptorString(name: String): Int = when (name) {
    "automation_name" -> R.string.automation_name
    "automation_enabled" -> R.string.automation_enabled
    "field_trigger_type" -> R.string.field_trigger_type
    "field_trigger_at" -> R.string.field_trigger_at
    "field_trigger_every_ms" -> R.string.field_trigger_every_ms
    "field_trigger_rrule" -> R.string.field_trigger_rrule
    "field_trigger_timezone" -> R.string.field_trigger_timezone
    "field_trigger_event_name" -> R.string.field_trigger_event_name
    "field_trigger_event_match" -> R.string.field_trigger_event_match
    "field_action_type" -> R.string.field_action_type
    "task_detail_tool" -> R.string.task_detail_tool
    "task_detail_action" -> R.string.task_detail_action
    "field_condition_source" -> R.string.field_condition_source
    "field_condition_key" -> R.string.field_condition_key
    "field_condition_operator" -> R.string.field_condition_operator
    "field_condition_value" -> R.string.field_condition_value
    "field_repeat_count" -> R.string.field_repeat_count
    "field_repeat_delay_ms" -> R.string.field_repeat_delay_ms
    "field_delay_duration_ms" -> R.string.field_delay_duration_ms
    "field_state_key" -> R.string.field_state_key
    "field_state_value" -> R.string.field_state_value
    "trigger_at" -> R.string.trigger_at
    "trigger_interval" -> R.string.trigger_interval
    "trigger_rrule" -> R.string.trigger_rrule
    "trigger_event" -> R.string.trigger_event
    "action_node_call" -> R.string.action_node_call
    "action_node_sequence" -> R.string.action_node_sequence
    "action_node_conditional" -> R.string.action_node_conditional
    "action_node_repeat" -> R.string.action_node_repeat
    "action_node_delay" -> R.string.action_node_delay
    "action_node_set_state" -> R.string.action_node_set_state
    else -> error("descriptor label resource $name is outside the S-UI-013 catalog")
}

/** S-UI-016 row trigger summary: the resource label of the trigger type option. */
@StringRes
private fun triggerLabel(type: String): Int? = when (type) {
    "at" -> R.string.trigger_at
    "interval" -> R.string.trigger_interval
    "rrule" -> R.string.trigger_rrule
    "event" -> R.string.trigger_event
    else -> null
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

private fun List<NodeStep>.tagPath(): String = if (isEmpty()) {
    "root"
} else {
    joinToString("/") { step ->
        when (step) {
            is NodeStep.Child -> "child${step.index}"
            NodeStep.Then -> "then"
            NodeStep.Else -> "else"
            NodeStep.Repeated -> "action"
        }
    }
}

private const val ROOT_TAG = "automation_editor"
private const val NOT_FOUND = "NOT_FOUND"
