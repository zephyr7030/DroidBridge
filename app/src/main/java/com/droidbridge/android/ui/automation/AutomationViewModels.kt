package com.droidbridge.android.ui.automation

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.droidbridge.android.product.automation.ACTION_PREFIX
import com.droidbridge.android.product.automation.ActionDraft
import com.droidbridge.android.product.automation.AutomationDescriptorCatalog
import com.droidbridge.android.product.automation.AutomationDraft
import com.droidbridge.android.product.automation.AutomationError
import com.droidbridge.android.product.automation.AutomationRepository
import com.droidbridge.android.product.automation.AutomationResult
import com.droidbridge.android.product.automation.AutomationRow
import com.droidbridge.android.product.automation.AutomationWire
import com.droidbridge.android.product.automation.BulkDeleteOutcome
import com.droidbridge.android.product.automation.DescriptorSection
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/** Presentation state only; the canonical list is requeried whenever the route is entered. */
data class AutomationListUiState(
    val rows: List<AutomationRow>? = null,
    val refreshing: Boolean = false,
    val loadFailed: Boolean = false,
    val deleteAllDialog: Boolean = false,
    val bulkDeleteActive: Boolean = false,
    val bulkResult: BulkDeleteOutcome? = null,
)

class AutomationListViewModel(private val repository: AutomationRepository) : ViewModel() {
    private val mutableState = MutableStateFlow(AutomationListUiState())
    val state: StateFlow<AutomationListUiState> = mutableState.asStateFlow()

    fun refresh() {
        viewModelScope.launch { load() }
    }

    /** The switch writes only the canonical enabled flag, then shows the requeried truth. */
    fun setEnabled(row: AutomationRow, enabled: Boolean) {
        viewModelScope.launch {
            // A rejected mutation (for example a revision conflict) is visible as the unchanged
            // canonical switch state the reload presents.
            repository.setEnabled(row, enabled)
            load()
        }
    }

    fun requestDeleteAll() {
        val current = state.value
        if (current.rows.isNullOrEmpty() || current.bulkDeleteActive) return
        mutableState.update { it.copy(deleteAllDialog = true) }
    }

    fun dismissDeleteAll() = mutableState.update { it.copy(deleteAllDialog = false) }

    fun confirmDeleteAll() {
        mutableState.update { it.copy(deleteAllDialog = false, bulkDeleteActive = true) }
        viewModelScope.launch {
            val outcome = repository.deleteAll()
            load()
            mutableState.update {
                it.copy(
                    bulkDeleteActive = false,
                    bulkResult = (outcome as? AutomationResult.Success)?.value,
                    loadFailed = it.loadFailed || outcome is AutomationResult.Failure,
                )
            }
        }
    }

    fun consumeBulkResult() = mutableState.update { it.copy(bulkResult = null) }

    private suspend fun load() {
        mutableState.update { it.copy(refreshing = true) }
        when (val listed = repository.list()) {
            is AutomationResult.Failure -> mutableState.update {
                // A failed refresh keeps the last immutable projection visible (S-UI-014).
                it.copy(refreshing = false, loadFailed = it.rows == null)
            }
            is AutomationResult.Success -> {
                val rows = listed.value.map { row ->
                    val fetched = repository.get(row.automationId) as? AutomationResult.Success
                    row.copy(
                        triggerType = fetched?.value?.get("trigger")?.jsonObject
                            ?.get("type")?.jsonPrimitive?.contentOrNull,
                    )
                }
                mutableState.update { it.copy(rows = rows, refreshing = false, loadFailed = false) }
            }
        }
    }
}

/** One step from an action node to a recursive action-tree slot. */
sealed interface NodeStep {
    data class Child(val index: Int) : NodeStep
    data object Then : NodeStep
    data object Else : NodeStep
    data object Repeated : NodeStep
}

data class AutomationEditorUiState(
    val draft: AutomationDraft? = null,
    val loadError: AutomationError? = null,
    val saving: Boolean = false,
    val validationError: AutomationError? = null,
    val deleteDialog: Boolean = false,
    val finished: Boolean = false,
)

class AutomationEditorViewModel(
    private val repository: AutomationRepository,
    private val catalog: AutomationDescriptorCatalog,
    private val automationId: String?,
) : ViewModel() {
    private val mutableState = MutableStateFlow(AutomationEditorUiState())
    val state: StateFlow<AutomationEditorUiState> = mutableState.asStateFlow()

    init {
        if (automationId == null) {
            mutableState.value = AutomationEditorUiState(draft = normalized(AutomationDraft.new()))
        } else {
            load(automationId)
        }
    }

    fun retry() {
        automationId?.let(::load)
    }

    fun setRootValue(path: String, value: JsonElement?) = editDraft { draft ->
        draft.copy(values = draft.values.with(path, value))
    }

    fun setNodeValue(ref: List<NodeStep>, path: String, value: JsonElement?) = editNode(ref) { node ->
        node.copy(values = node.values.with(path, value))
    }

    /** A new action type starts a fresh node, discarding the inactive variant's input. */
    fun changeNodeType(ref: List<NodeStep>, type: String) = editNode(ref) { node ->
        if (node.type == type) node else ActionDraft.ofType(type)
    }

    fun addChild(ref: List<NodeStep>, type: String) = editNode(ref) { node ->
        node.copy(children = node.children + ActionDraft.ofType(type))
    }

    fun moveChild(ref: List<NodeStep>, index: Int, delta: Int) = editNode(ref) { node ->
        val target = index + delta
        if (target !in node.children.indices) return@editNode node
        node.copy(
            children = node.children.toMutableList().also { children ->
                children[index] = node.children[target]
                children[target] = node.children[index]
            },
        )
    }

    fun removeChild(ref: List<NodeStep>, index: Int) = editNode(ref) { node ->
        node.copy(children = node.children.filterIndexed { position, _ -> position != index })
    }

    fun setSlot(ref: List<NodeStep>, slot: NodeStep, type: String?) = editNode(ref) { node ->
        val child = type?.let(ActionDraft::ofType)
        when (slot) {
            NodeStep.Then -> node.copy(thenAction = child)
            NodeStep.Else -> node.copy(elseAction = child)
            NodeStep.Repeated -> node.copy(repeatedAction = child)
            is NodeStep.Child -> node
        }
    }

    /** Saves exactly the visible descriptor fields; the Contract result owns every validation. */
    fun save() {
        val draft = state.value.draft ?: return
        if (state.value.saving) return
        mutableState.update { it.copy(saving = true, validationError = null) }
        viewModelScope.launch {
            when (val saved = repository.save(AutomationWire.saveInput(catalog, draft))) {
                is AutomationResult.Success -> mutableState.update { it.copy(saving = false, finished = true) }
                is AutomationResult.Failure -> mutableState.update {
                    it.copy(saving = false, validationError = saved.error)
                }
            }
        }
    }

    fun requestDelete() {
        if (state.value.draft?.automationId != null) mutableState.update { it.copy(deleteDialog = true) }
    }

    fun dismissDelete() = mutableState.update { it.copy(deleteDialog = false) }

    fun confirmDelete() {
        val draft = state.value.draft ?: return
        val id = draft.automationId ?: return
        val revision = draft.expectedRevision ?: return
        mutableState.update { it.copy(deleteDialog = false, saving = true) }
        viewModelScope.launch {
            when (val deleted = repository.delete(id, revision)) {
                is AutomationResult.Success -> mutableState.update { it.copy(saving = false, finished = true) }
                is AutomationResult.Failure -> mutableState.update {
                    it.copy(saving = false, validationError = deleted.error)
                }
            }
        }
    }

    private fun load(id: String) {
        mutableState.update { it.copy(loadError = null) }
        viewModelScope.launch {
            when (val fetched = repository.get(id)) {
                is AutomationResult.Success -> mutableState.update {
                    it.copy(draft = normalized(AutomationWire.draftOf(fetched.value)), loadError = null)
                }
                is AutomationResult.Failure -> mutableState.update { it.copy(loadError = fetched.error) }
            }
        }
    }

    private fun editDraft(transform: (AutomationDraft) -> AutomationDraft) = mutableState.update { current ->
        val draft = current.draft ?: return@update current
        current.copy(draft = normalized(transform(draft)))
    }

    private fun editNode(ref: List<NodeStep>, transform: (ActionDraft) -> ActionDraft) = editDraft { draft ->
        draft.copy(action = draft.action.updated(ref, transform))
    }

    /** Fills schema defaults of every newly visible field without replacing user input. */
    private fun normalized(draft: AutomationDraft): AutomationDraft {
        val root = catalog.withDefaults(setOf(DescriptorSection.Root, DescriptorSection.Trigger), draft.values)
        return draft.copy(values = root, action = normalizedNode(root, draft.action))
    }

    private fun normalizedNode(root: Map<String, JsonElement>, node: ActionDraft): ActionDraft = node.copy(
        values = catalog.withDefaults(
            setOf(DescriptorSection.Action, DescriptorSection.CallArguments),
            root + node.values,
        ).filterKeys { it.startsWith(ACTION_PREFIX) },
        children = node.children.map { normalizedNode(root, it) },
        thenAction = node.thenAction?.let { normalizedNode(root, it) },
        elseAction = node.elseAction?.let { normalizedNode(root, it) },
        repeatedAction = node.repeatedAction?.let { normalizedNode(root, it) },
    )
}

private fun Map<String, JsonElement>.with(path: String, value: JsonElement?): Map<String, JsonElement> =
    if (value == null) this - path else this + (path to value)

private fun ActionDraft.updated(ref: List<NodeStep>, transform: (ActionDraft) -> ActionDraft): ActionDraft {
    if (ref.isEmpty()) return transform(this)
    val rest = ref.drop(1)
    return when (val step = ref.first()) {
        is NodeStep.Child -> copy(
            children = children.mapIndexed { index, child ->
                if (index == step.index) child.updated(rest, transform) else child
            },
        )
        NodeStep.Then -> copy(thenAction = thenAction?.updated(rest, transform))
        NodeStep.Else -> copy(elseAction = elseAction?.updated(rest, transform))
        NodeStep.Repeated -> copy(repeatedAction = repeatedAction?.updated(rest, transform))
    }
}

/** The persistent-state keys the draft reads or writes, shown read-only (S-UI-008). */
fun referencedStateKeys(draft: AutomationDraft): List<String> {
    val keys = sortedSetOf<String>()
    fun visit(node: ActionDraft) {
        when (node.type) {
            "set_state" -> node.values.stringAt("/action/set_state/key")?.let(keys::add)
            "conditional" -> if (node.values.stringAt("/action/conditional/condition/source") == "state") {
                node.values.stringAt("/action/conditional/condition/key")?.let(keys::add)
            }
        }
        node.children.forEach(::visit)
        listOfNotNull(node.thenAction, node.elseAction, node.repeatedAction).forEach(::visit)
    }
    visit(draft.action)
    return keys.filter(String::isNotEmpty)
}

private fun Map<String, JsonElement>.stringAt(path: String): String? =
    (get(path) as? JsonPrimitive)?.takeIf { it.isString }?.content

internal fun JsonElement?.asObjectOrEmpty(): JsonObject = this as? JsonObject ?: JsonObject(emptyMap())
