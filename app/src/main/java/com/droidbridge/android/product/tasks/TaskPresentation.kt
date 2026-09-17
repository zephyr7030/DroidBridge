package com.droidbridge.android.product.tasks

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.Locale
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive

/** The S-UI-007 Tasks filter; `All` sends no state filter. */
enum class TaskFilter(val states: List<String>?) {
    All(null),
    Active(listOf("created", "queued", "running")),
    Completed(listOf("completed", "failed", "cancelled", "interrupted")),
}

/** One `task_control.list` summary exactly as the Runtime returned it. */
data class TaskSummary(
    val taskId: String,
    val state: String,
    val tool: String,
    val action: String,
    val createdAt: String,
    val startedAt: String?,
    val endedAt: String?,
)

/** One `task_control.get|cancel` snapshot exactly as the Runtime returned it. */
data class TaskSnapshot(
    val taskId: String,
    val state: String,
    val tool: String,
    val action: String,
    val createdAt: String,
    val startedAt: String?,
    val endedAt: String?,
    val executionClass: String?,
    val cancelRequested: Boolean,
    val result: JsonElement?,
    val error: JsonElement?,
)

object TaskPresentation {
    private val activeStates = TaskFilter.Active.states.orEmpty().toSet()
    private val outputRefFields =
        listOf("stdout_ref", "stderr_ref", "data_ref", "image_ref", "capture_ref", "packet_ref")

    /** S-UI-007: `Cancel task` exists exactly for an active state without a recorded request. */
    fun cancellable(snapshot: TaskSnapshot): Boolean =
        snapshot.state in activeStates && !snapshot.cancelRequested

    /** S-UI-017 output-link rows: top-level ref strings of the Task result in fixed field order. */
    fun outputRefs(result: JsonElement?): List<String> {
        val fields = result as? JsonObject ?: return emptyList()
        return outputRefFields.mapNotNull { field ->
            (fields[field] as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content
        }
    }

    /** S-UI-017 time rendering: the canonical instant in the device zone, medium localized style. */
    fun formatInstant(rfc3339: String, zone: ZoneId, locale: Locale): String =
        DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM)
            .withLocale(locale)
            .withZone(zone)
            .format(Instant.parse(rfc3339))

    /** S-UI-017 Result/Error rendering: the exact JSON value with two-space indentation. */
    fun prettyJson(value: JsonElement): String = StringBuilder().also { write(value, 0, it) }.toString()

    private fun write(value: JsonElement, depth: Int, out: StringBuilder) {
        when (value) {
            is JsonObject -> writeContainer(value.entries.toList(), '{', '}', depth, out) { (key, element) ->
                out.append(JsonPrimitive(key).toString()).append(": ")
                write(element, depth + 1, out)
            }
            is JsonArray -> writeContainer(value.toList(), '[', ']', depth, out) { element ->
                write(element, depth + 1, out)
            }
            else -> out.append(value.toString())
        }
    }

    private fun <T> writeContainer(
        items: List<T>,
        open: Char,
        close: Char,
        depth: Int,
        out: StringBuilder,
        writeItem: (T) -> Unit,
    ) {
        if (items.isEmpty()) {
            out.append(open).append(close)
            return
        }
        out.append(open).append('\n')
        items.forEachIndexed { index, item ->
            indent(depth + 1, out)
            writeItem(item)
            if (index != items.lastIndex) out.append(',')
            out.append('\n')
        }
        indent(depth, out)
        out.append(close)
    }

    private fun indent(depth: Int, out: StringBuilder) {
        repeat(depth) { out.append("  ") }
    }
}
