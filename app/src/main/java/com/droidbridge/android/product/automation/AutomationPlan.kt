package com.droidbridge.android.product.automation

import java.time.DayOfWeek
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.DateTimeParseException
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

/** When a template Automation runs. Every variant maps to exactly one saved trigger. */
sealed interface PlanTrigger {
    data class Daily(val time: LocalTime) : PlanTrigger
    data class Weekly(val days: Set<DayOfWeek>, val time: LocalTime) : PlanTrigger
    data class Every(val minutes: Long) : PlanTrigger
    data class Once(val at: LocalDateTime) : PlanTrigger
    data object RuntimeStarted : PlanTrigger
    /** A default-network change; a null transport accepts every network. */
    data class NetworkChanged(val transport: String?) : PlanTrigger
}

enum class ElementBy(val wire: String) {
    Text("text"),
    TextContains("text_contains"),
    Description("description"),
    ResourceId("resource_id"),
}

enum class PlanKey(val keyCode: Int) {
    Back(4),
    Home(3),
    Recents(187),
    Enter(66),
}

enum class PlanRunAs(val wire: String) { App("app"), Shell("shell"), Root("root") }

/** One template step. [continueOnFailure] lets the run go on when this step fails. */
sealed interface PlanStep {
    val continueOnFailure: Boolean

    data class OpenApp(val packageName: String, override val continueOnFailure: Boolean = false) : PlanStep
    data class TapElement(
        val by: ElementBy,
        val value: String,
        val longPress: Boolean = false,
        val waitSeconds: Int = DEFAULT_WAIT_SECONDS,
        override val continueOnFailure: Boolean = false,
    ) : PlanStep
    /** Types into the element matched by [target], or into the focused editor when it is null. */
    data class TypeText(
        val text: String,
        val target: String? = null,
        val waitSeconds: Int = DEFAULT_WAIT_SECONDS,
        override val continueOnFailure: Boolean = false,
    ) : PlanStep
    data class PressKey(val key: PlanKey, override val continueOnFailure: Boolean = false) : PlanStep
    data class Wait(val seconds: Int) : PlanStep {
        override val continueOnFailure: Boolean get() = false
    }
    data class RunCommand(
        val command: String,
        val runAs: PlanRunAs = PlanRunAs.App,
        override val continueOnFailure: Boolean = false,
    ) : PlanStep
    data class CopyText(val text: String, override val continueOnFailure: Boolean = false) : PlanStep

    companion object {
        const val DEFAULT_WAIT_SECONDS = 10
        const val MAX_WAIT_SECONDS = 60
    }
}

data class AutomationPlan(
    val name: String,
    val enabled: Boolean,
    val trigger: PlanTrigger,
    val steps: List<PlanStep>,
)

/**
 * The template editor's only mapping to saved definitions. [decode] returns null for anything a
 * template cannot represent exactly, so such an Automation is shown read-only instead of being
 * rewritten by a lossy edit.
 */
object AutomationPlans {
    fun encode(plan: AutomationPlan, zone: ZoneId, today: LocalDate): JsonObject = buildJsonObject {
        put("name", plan.name)
        put("enabled", plan.enabled)
        put("trigger", encodeTrigger(plan.trigger, zone, today))
        put("action", buildJsonObject {
            put("type", "sequence")
            put("children", buildJsonArray { plan.steps.forEach { add(encodeStep(it)) } })
        })
    }

    fun decode(automation: JsonObject): AutomationPlan? {
        val trigger = decodeTrigger(automation.obj("trigger") ?: return null) ?: return null
        val action = automation.obj("action") ?: return null
        val nodes = if (action.str("type") == "sequence") {
            (action["children"] as? JsonArray)?.map { it as? JsonObject ?: return null } ?: return null
        } else {
            listOf(action)
        }
        val steps = nodes.map { decodeStep(it) ?: return null }
        return AutomationPlan(
            name = automation.str("name") ?: return null,
            enabled = (automation["enabled"] as? JsonPrimitive)?.booleanOrNull ?: return null,
            trigger = trigger,
            steps = steps,
        )
    }

    private fun encodeTrigger(trigger: PlanTrigger, zone: ZoneId, today: LocalDate): JsonObject = buildJsonObject {
        when (trigger) {
            is PlanTrigger.Daily -> {
                put("type", "rrule")
                put("rrule", rrule(today, trigger.time, "FREQ=DAILY"))
                put("timezone", zone.id)
            }
            is PlanTrigger.Weekly -> {
                put("type", "rrule")
                val days = DayOfWeek.entries.filter { it in trigger.days }.joinToString(",") { it.rruleDay() }
                put("rrule", rrule(today, trigger.time, "FREQ=WEEKLY;BYDAY=$days"))
                put("timezone", zone.id)
            }
            is PlanTrigger.Every -> {
                put("type", "interval")
                put("every_ms", trigger.minutes * 60_000)
            }
            is PlanTrigger.Once -> {
                put("type", "at")
                put("at", trigger.at.atZone(zone).toOffsetDateTime().format(DateTimeFormatter.ISO_OFFSET_DATE_TIME))
            }
            PlanTrigger.RuntimeStarted -> {
                put("type", "event")
                put("name", RUNTIME_READY)
            }
            is PlanTrigger.NetworkChanged -> {
                put("type", "event")
                put("name", NETWORK_CHANGED)
                trigger.transport?.let { transport -> put("match", buildJsonObject { put("transport", transport) }) }
            }
        }
    }

    fun decodeTrigger(trigger: JsonObject): PlanTrigger? = when (trigger.str("type")) {
        "interval" -> trigger.long("every_ms")?.takeIf { it % 60_000 == 0L }?.let { PlanTrigger.Every(it / 60_000) }
        "at" -> trigger.str("at")?.let { at ->
            try {
                PlanTrigger.Once(OffsetDateTime.parse(at).atZoneSameInstant(ZoneId.systemDefault()).toLocalDateTime())
            } catch (_: DateTimeParseException) {
                null
            }
        }
        "rrule" -> decodeRrule(trigger.str("rrule") ?: "")
        "event" -> when (trigger.str("name")) {
            RUNTIME_READY -> PlanTrigger.RuntimeStarted.takeIf { trigger["match"] == null }
            NETWORK_CHANGED -> {
                val match = trigger.obj("match")
                when {
                    match == null -> PlanTrigger.NetworkChanged(null)
                    match.keys == setOf("transport") -> match.str("transport")
                        ?.takeIf { it in NETWORK_TRANSPORTS }
                        ?.let { PlanTrigger.NetworkChanged(it) }
                    else -> null
                }
            }
            else -> null
        }
        else -> null
    }

    private fun rrule(today: LocalDate, time: LocalTime, rule: String): String =
        "DTSTART:${today.atTime(time.withSecond(0).withNano(0)).format(DTSTART)}\nRRULE:$rule"

    private fun decodeRrule(text: String): PlanTrigger? {
        val lines = text.replace("\r\n", "\n").split('\n')
        if (lines.size != 2) return null
        val start = lines[0].removePrefix("DTSTART:").takeIf { it != lines[0] } ?: return null
        val rule = lines[1].removePrefix("RRULE:").takeIf { it != lines[1] } ?: return null
        val time = try {
            LocalDateTime.parse(start, DTSTART).toLocalTime()
        } catch (_: DateTimeParseException) {
            return null
        }
        if (time.second != 0) return null
        if (rule == "FREQ=DAILY") return PlanTrigger.Daily(time)
        val days = rule.removePrefix("FREQ=WEEKLY;BYDAY=").takeIf { it != rule } ?: return null
        val parsed = days.split(',').map { token -> DayOfWeek.entries.firstOrNull { it.rruleDay() == token } ?: return null }
        return if (parsed.isEmpty() || parsed.toSet().size != parsed.size) null else PlanTrigger.Weekly(parsed.toSet(), time)
    }

    private fun encodeStep(step: PlanStep): JsonObject = when (step) {
        is PlanStep.Wait -> buildJsonObject {
            put("type", "delay")
            put("duration_ms", step.seconds * 1_000L)
        }
        else -> buildJsonObject {
            put("type", "call")
            when (step) {
                is PlanStep.OpenApp -> call("android", "launch") {
                    put("operation", "package")
                    put("package_name", step.packageName)
                }
                is PlanStep.TapElement -> call("visual", "element") {
                    put("operation", if (step.longPress) "long_press" else "tap")
                    put("by", step.by.wire)
                    put("value", step.value)
                    put("wait_ms", step.waitSeconds * 1_000L)
                }
                is PlanStep.TypeText -> if (step.target == null) {
                    call("visual", "interact") {
                        put("operation", "text")
                        put("text", step.text)
                    }
                } else {
                    call("visual", "element") {
                        put("operation", "text")
                        put("by", ElementBy.TextContains.wire)
                        put("value", step.target)
                        put("text", step.text)
                        put("wait_ms", step.waitSeconds * 1_000L)
                    }
                }
                is PlanStep.PressKey -> call("visual", "interact") {
                    put("operation", "key")
                    put("key_code", step.key.keyCode)
                    put("meta_state", 0)
                }
                is PlanStep.RunCommand -> call("command", "run") {
                    put("command", step.command)
                    put("run_as", step.runAs.wire)
                }
                is PlanStep.CopyText -> call("android", "clipboard") {
                    put("operation", "write")
                    put("text", step.text)
                }
                is PlanStep.Wait -> error("handled above")
            }
            if (step.continueOnFailure) put("on_failure", "continue")
        }
    }

    private fun kotlinx.serialization.json.JsonObjectBuilder.call(
        tool: String,
        action: String,
        args: kotlinx.serialization.json.JsonObjectBuilder.() -> Unit,
    ) {
        put("tool", tool)
        put("action", action)
        put("args", buildJsonObject(args))
    }

    private fun decodeStep(node: JsonObject): PlanStep? {
        when (node.str("type")) {
            "delay" -> {
                val ms = node.long("duration_ms") ?: return null
                return if (ms % 1_000 == 0L) PlanStep.Wait((ms / 1_000).toInt()) else null
            }
            "call" -> {}
            else -> return null
        }
        val continueOnFailure = when (node.str("on_failure")) {
            null, "stop" -> false
            "continue" -> true
            else -> return null
        }
        val args = node.obj("args") ?: return null
        return when ("${node.str("tool")}.${node.str("action")}") {
            "android.launch" -> args.only("operation", "package_name")
                ?.takeIf { it.str("operation") == "package" }
                ?.str("package_name")
                ?.let { PlanStep.OpenApp(it, continueOnFailure) }
            "android.clipboard" -> args.only("operation", "text")
                ?.takeIf { it.str("operation") == "write" }
                ?.str("text")
                ?.let { PlanStep.CopyText(it, continueOnFailure) }
            "command.run" -> decodeCommand(args, continueOnFailure)
            "visual.element" -> decodeElement(args, continueOnFailure)
            "visual.interact" -> when (args.str("operation")) {
                "text" -> args.only("operation", "text")?.str("text")?.let { PlanStep.TypeText(it, null, continueOnFailure = continueOnFailure) }
                "key" -> args.only("operation", "key_code", "meta_state")
                    ?.takeIf { it.long("meta_state") == 0L }
                    ?.let { key -> PlanKey.entries.firstOrNull { it.keyCode.toLong() == key.long("key_code") } }
                    ?.let { PlanStep.PressKey(it, continueOnFailure) }
                else -> null
            }
            else -> null
        }
    }

    private fun decodeCommand(args: JsonObject, continueOnFailure: Boolean): PlanStep? {
        // A command saved by a template carries only the Contract defaults it did not set.
        if (args.keys - COMMAND_KEYS != emptySet<String>()) return null
        if ((args["as_task"] as? JsonPrimitive)?.booleanOrNull == true) return null
        if (args.long("timeout_ms") != null && args.long("timeout_ms") != DEFAULT_COMMAND_TIMEOUT_MS) return null
        if (args.long("max_output_bytes") != null && args.long("max_output_bytes") != DEFAULT_COMMAND_OUTPUT_BYTES) return null
        val runAs = PlanRunAs.entries.firstOrNull { it.wire == args.str("run_as") } ?: return null
        return PlanStep.RunCommand(args.str("command") ?: return null, runAs, continueOnFailure)
    }

    private fun decodeElement(args: JsonObject, continueOnFailure: Boolean): PlanStep? {
        val waitMs = args.long("wait_ms") ?: return null
        if (waitMs % 1_000 != 0L) return null
        val wait = (waitMs / 1_000).toInt()
        val by = ElementBy.entries.firstOrNull { it.wire == args.str("by") } ?: return null
        val value = args.str("value") ?: return null
        return when (args.str("operation")) {
            "tap", "long_press" -> args.only("operation", "by", "value", "wait_ms")?.let {
                PlanStep.TapElement(by, value, args.str("operation") == "long_press", wait, continueOnFailure)
            }
            "text" -> args.only("operation", "by", "value", "text", "wait_ms")
                ?.takeIf { by == ElementBy.TextContains }
                ?.str("text")
                ?.let { PlanStep.TypeText(it, value, wait, continueOnFailure) }
            else -> null
        }
    }

    private fun DayOfWeek.rruleDay(): String = name.take(2)

    private fun JsonObject.str(key: String): String? = (this[key] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull
    private fun JsonObject.long(key: String): Long? = (this[key] as? JsonPrimitive)?.takeIf { !it.isString }?.longOrNull
    private fun JsonObject.obj(key: String): JsonObject? = this[key] as? JsonObject
    private fun JsonObject.only(vararg keys: String): JsonObject? = takeIf { this.keys == keys.toSet() }

    private val DTSTART: DateTimeFormatter = DateTimeFormatter.ofPattern("yyyyMMdd'T'HHmmss")
    private val COMMAND_KEYS = setOf("command", "run_as", "timeout_ms", "max_output_bytes", "as_task")
    private const val DEFAULT_COMMAND_TIMEOUT_MS = 30_000L
    private const val DEFAULT_COMMAND_OUTPUT_BYTES = 65_536L
    const val RUNTIME_READY = "runtime.ready"
    const val NETWORK_CHANGED = "network.default_changed"
    val NETWORK_TRANSPORTS = setOf("wifi", "cellular")
}

/** A read-only line of any saved action tree, including ones no template can edit. */
sealed interface StepLine {
    val depth: Int

    data class Step(override val depth: Int, val step: PlanStep) : StepLine
    data class Call(override val depth: Int, val tool: String, val action: String) : StepLine
    data class If(override val depth: Int, val source: String, val key: String, val operator: String, val value: String?) : StepLine
    data class Otherwise(override val depth: Int) : StepLine
    data class Repeat(override val depth: Int, val count: Int) : StepLine
    data class SetState(override val depth: Int, val key: String, val value: String) : StepLine
}

object AutomationSteps {
    fun describe(action: JsonObject, depth: Int = 0): List<StepLine> = when (action.string("type")) {
        "sequence" -> (action["children"] as? JsonArray).orEmpty().filterIsInstance<JsonObject>().flatMap { describe(it, depth) }
        "conditional" -> buildList {
            val condition = action["condition"] as? JsonObject
            add(
                StepLine.If(
                    depth,
                    condition?.string("source").orEmpty(),
                    condition?.string("key").orEmpty(),
                    condition?.string("operator").orEmpty(),
                    (condition?.get("value") as? JsonPrimitive)?.contentOrNull,
                ),
            )
            (action["then"] as? JsonObject)?.let { addAll(describe(it, depth + 1)) }
            (action["else"] as? JsonObject)?.let {
                add(StepLine.Otherwise(depth))
                addAll(describe(it, depth + 1))
            }
        }
        "repeat" -> buildList {
            add(StepLine.Repeat(depth, (action["count"] as? JsonPrimitive)?.intOrNull ?: 0))
            (action["action"] as? JsonObject)?.let { addAll(describe(it, depth + 1)) }
        }
        "set_state" -> listOf(
            StepLine.SetState(depth, action.string("key").orEmpty(), (action["value"] as? JsonElement)?.toString().orEmpty()),
        )
        else -> {
            val step = AutomationPlans.decode(
                buildJsonObject {
                    put("name", "_")
                    put("enabled", true)
                    put("trigger", buildJsonObject { put("type", "event"); put("name", AutomationPlans.RUNTIME_READY) })
                    put("action", action)
                },
            )?.steps?.singleOrNull()
            listOf(step?.let { StepLine.Step(depth, it) } ?: StepLine.Call(depth, action.string("tool").orEmpty(), action.string("action").orEmpty()))
        }
    }

    private fun JsonObject.string(key: String): String? = (this[key] as? JsonPrimitive)?.contentOrNull
}
