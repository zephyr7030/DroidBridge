package com.droidbridge.android.ui.automation

import com.droidbridge.android.product.runtime.PublicError
import com.droidbridge.android.product.runtime.PublicResult
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.droidbridge.android.product.automation.AutomationDetail
import com.droidbridge.android.product.automation.AutomationPlan
import com.droidbridge.android.product.automation.AutomationPlans
import com.droidbridge.android.product.automation.AutomationRepository
import com.droidbridge.android.product.automation.AutomationRow
import com.droidbridge.android.product.automation.BulkDeleteOutcome
import com.droidbridge.android.product.automation.PlanStep
import com.droidbridge.android.product.automation.PlanTrigger
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.ZoneId
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put

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
        viewModelScope.launch {
            // A Runtime that is still starting answers nothing yet, so the first list is retried
            // for as long as a start takes before the page reports it cannot be read.
            repeat(STARTUP_ATTEMPTS) { attempt ->
                load()
                if (state.value.rows != null) return@launch
                if (attempt + 1 < STARTUP_ATTEMPTS) delay(STARTUP_RETRY_MILLIS)
            }
        }
    }

    /** The switch writes only the canonical enabled flag, then shows the requeried truth. */
    fun setEnabled(row: AutomationRow, enabled: Boolean) {
        viewModelScope.launch {
            repository.setEnabled(row.automationId, row.revision, enabled)
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
                    bulkResult = (outcome as? PublicResult.Success)?.value,
                    loadFailed = it.loadFailed || outcome is PublicResult.Failure,
                )
            }
        }
    }

    fun consumeBulkResult() = mutableState.update { it.copy(bulkResult = null) }

    private companion object {
        const val STARTUP_ATTEMPTS = 30
        const val STARTUP_RETRY_MILLIS = 1_000L
    }

    private suspend fun load() {
        mutableState.update { it.copy(refreshing = true) }
        when (val listed = repository.list()) {
            is PublicResult.Failure -> mutableState.update {
                // A failed refresh keeps the last immutable projection visible (S-UI-014).
                it.copy(refreshing = false, loadFailed = it.rows == null)
            }
            is PublicResult.Success -> {
                val rows = listed.value.map { row ->
                    val fetched = repository.get(row.automationId) as? PublicResult.Success
                    row.copy(trigger = fetched?.value?.automation?.get("trigger")?.jsonObject)
                }
                mutableState.update { it.copy(rows = rows, refreshing = false, loadFailed = false) }
            }
        }
    }
}

data class AutomationDetailUiState(
    val detail: AutomationDetail? = null,
    val loadError: PublicError? = null,
    val runStarted: Boolean = false,
    val runError: PublicError? = null,
    val deleteDialog: Boolean = false,
    val deleted: Boolean = false,
) {
    /** The template this Automation is editable as, or null when only an AI can change it. */
    val plan: AutomationPlan? get() = detail?.let { AutomationPlans.decode(it.automation) }
}

class AutomationDetailViewModel(
    private val repository: AutomationRepository,
    private val automationId: String,
) : ViewModel() {
    private val mutableState = MutableStateFlow(AutomationDetailUiState())
    val state: StateFlow<AutomationDetailUiState> = mutableState.asStateFlow()

    fun refresh() {
        viewModelScope.launch { load() }
    }

    fun setEnabled(enabled: Boolean) {
        val detail = state.value.detail ?: return
        viewModelScope.launch {
            repository.setEnabled(detail.automationId, detail.revision, enabled)
            load()
        }
    }

    /** Requests one run, then follows the history while that run is admitted and settles. */
    fun runNow() {
        viewModelScope.launch {
            when (val requested = repository.run(automationId)) {
                is PublicResult.Failure -> mutableState.update { it.copy(runError = requested.error) }
                is PublicResult.Success -> {
                    mutableState.update { it.copy(runStarted = true) }
                    val previous = state.value.detail?.history?.firstOrNull()?.taskId
                    repeat(RUN_FOLLOW_POLLS) {
                        delay(RUN_FOLLOW_INTERVAL_MS)
                        load()
                        val newest = state.value.detail?.history?.firstOrNull()
                        if (newest != null && newest.taskId != previous && newest.state !in ACTIVE_STATES) {
                            return@launch
                        }
                    }
                }
            }
        }
    }

    fun consumeRunFeedback() = mutableState.update { it.copy(runStarted = false, runError = null) }

    fun consumeLeave() = mutableState.update { it.copy(deleted = false, loadError = null) }

    fun requestDelete() = mutableState.update { it.copy(deleteDialog = true) }

    fun dismissDelete() = mutableState.update { it.copy(deleteDialog = false) }

    fun confirmDelete() {
        val detail = state.value.detail ?: return
        mutableState.update { it.copy(deleteDialog = false) }
        viewModelScope.launch {
            when (val deleted = repository.delete(detail.automationId, detail.revision)) {
                is PublicResult.Success -> mutableState.update { it.copy(deleted = true) }
                is PublicResult.Failure -> {
                    mutableState.update { it.copy(runError = deleted.error) }
                    load()
                }
            }
        }
    }

    private suspend fun load() {
        when (val fetched = repository.get(automationId, HISTORY_LIMIT)) {
            is PublicResult.Success -> mutableState.update { it.copy(detail = fetched.value, loadError = null) }
            is PublicResult.Failure -> mutableState.update { it.copy(loadError = fetched.error) }
        }
    }

    private companion object {
        const val HISTORY_LIMIT = 20
        const val RUN_FOLLOW_POLLS = 30
        const val RUN_FOLLOW_INTERVAL_MS = 1_000L
        val ACTIVE_STATES = setOf("queued", "running")
    }
}

/** A problem the editor shows before saving; the Runtime still validates everything it saves. */
sealed interface PlanProblem {
    data object NameRequired : PlanProblem
    data object StepsRequired : PlanProblem
    data class StepIncomplete(val number: Int) : PlanProblem
    data object OncePast : PlanProblem
    data object WeeklyDays : PlanProblem
    data object Interval : PlanProblem
    data class SaveFailed(val error: PublicError) : PlanProblem
}

data class AutomationEditorUiState(
    val plan: AutomationPlan? = null,
    /** The plan as loaded or last saved, so leaving with changes asks first. */
    val saved: AutomationPlan? = null,
    val loadError: PublicError? = null,
    val problem: PlanProblem? = null,
    val saving: Boolean = false,
    val finished: Boolean = false,
) {
    val dirty: Boolean get() = plan != null && plan != saved
}

class AutomationEditorViewModel(
    private val repository: AutomationRepository,
    private val automationId: String?,
    private val zone: () -> ZoneId = ZoneId::systemDefault,
    private val now: () -> LocalDateTime = LocalDateTime::now,
) : ViewModel() {
    private var revision: Long? = null
    private val mutableState = MutableStateFlow(AutomationEditorUiState())
    val state: StateFlow<AutomationEditorUiState> = mutableState.asStateFlow()

    init {
        if (automationId == null) {
            val blank = AutomationPlan(
                name = "",
                enabled = true,
                trigger = PlanTrigger.Daily(LocalTime.of(8, 0)),
                steps = emptyList(),
            )
            mutableState.update { it.copy(plan = blank, saved = blank) }
        } else {
            retry()
        }
    }

    fun retry() {
        val id = automationId ?: return
        viewModelScope.launch {
            when (val fetched = repository.get(id)) {
                is PublicResult.Failure -> mutableState.update { it.copy(loadError = fetched.error) }
                is PublicResult.Success -> {
                    revision = fetched.value.revision
                    val plan = AutomationPlans.decode(fetched.value.automation)
                    mutableState.update { it.copy(plan = plan, saved = plan, loadError = null) }
                }
            }
        }
    }

    fun consumeFinished() = mutableState.update { it.copy(finished = false) }

    fun update(change: (AutomationPlan) -> AutomationPlan) =
        mutableState.update { state -> state.copy(plan = state.plan?.let(change), problem = null) }

    fun setStep(index: Int, step: PlanStep) = update { plan ->
        plan.copy(steps = plan.steps.toMutableList().also { it[index] = step })
    }

    fun addStep(step: PlanStep) = update { plan -> plan.copy(steps = plan.steps + step) }

    fun removeStep(index: Int) = update { plan -> plan.copy(steps = plan.steps.filterIndexed { i, _ -> i != index }) }

    fun moveStep(index: Int, offset: Int) = update { plan ->
        val target = index + offset
        if (target !in plan.steps.indices) return@update plan
        plan.copy(steps = plan.steps.toMutableList().also { steps -> steps.add(target, steps.removeAt(index)) })
    }

    fun save() {
        val plan = state.value.plan ?: return
        val problem = problemOf(plan)
        if (problem != null) {
            mutableState.update { it.copy(problem = problem) }
            return
        }
        mutableState.update { it.copy(saving = true) }
        viewModelScope.launch {
            val definition = AutomationPlans.encode(plan, zone(), now().toLocalDate())
            val input: JsonObject = revision?.let { expected ->
                buildJsonObject {
                    put("automation_id", automationId)
                    put("expected_revision", expected)
                    definition.forEach { (key, value) -> put(key, value) }
                }
            } ?: definition
            when (val saved = repository.save(input)) {
                is PublicResult.Success -> mutableState.update {
                    it.copy(saving = false, saved = plan, finished = true)
                }
                is PublicResult.Failure -> mutableState.update {
                    it.copy(saving = false, problem = PlanProblem.SaveFailed(saved.error))
                }
            }
        }
    }

    private fun problemOf(plan: AutomationPlan): PlanProblem? {
        if (plan.name.isBlank()) return PlanProblem.NameRequired
        when (val trigger = plan.trigger) {
            is PlanTrigger.Weekly -> if (trigger.days.isEmpty()) return PlanProblem.WeeklyDays
            is PlanTrigger.Every -> if (trigger.minutes < 1) return PlanProblem.Interval
            is PlanTrigger.Once -> if (!trigger.at.isAfter(now())) return PlanProblem.OncePast
            else -> {}
        }
        if (plan.steps.isEmpty()) return PlanProblem.StepsRequired
        plan.steps.forEachIndexed { index, step ->
            if (!complete(step)) return PlanProblem.StepIncomplete(index + 1)
        }
        return null
    }

    private fun complete(step: PlanStep): Boolean = when (step) {
        is PlanStep.OpenApp -> step.packageName.isNotBlank()
        is PlanStep.TapElement -> step.value.isNotBlank() && step.waitSeconds in 0..PlanStep.MAX_WAIT_SECONDS
        is PlanStep.TypeText -> step.target?.isNotBlank() != false && step.waitSeconds in 0..PlanStep.MAX_WAIT_SECONDS
        is PlanStep.PressKey -> true
        is PlanStep.Wait -> step.seconds in 1..MAX_WAIT_STEP_SECONDS
        is PlanStep.RunCommand -> step.command.isNotBlank()
        is PlanStep.CopyText -> true
    }

    private companion object {
        /** The Contract's delay bound of one day. */
        const val MAX_WAIT_STEP_SECONDS = 86_400
    }
}
