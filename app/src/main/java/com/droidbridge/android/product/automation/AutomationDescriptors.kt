package com.droidbridge.android.product.automation

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

enum class DescriptorControl(val wire: String) {
    Switch("switch"),
    OutlinedText("outlined_text"),
    SingleChoice("single_choice"),
    Optional("optional"),
    ScalarEditor("scalar_editor"),
    KeyScalarTable("key_scalar_table"),
}

enum class DescriptorValueKind(val wire: String) {
    Boolean("boolean"),
    String("string"),
    Integer("integer"),
    Scalar("scalar"),
}

/** A choice; a null [labelResource] displays the exact wire [value]. */
data class DescriptorOption(val value: String, val labelResource: String?)

data class DescriptorCondition(val canonicalPath: String, val values: List<JsonPrimitive>)

/** One closed S-UI-008 field descriptor, consumed as data and never inferred from a schema. */
data class FieldDescriptor(
    val canonicalPath: String,
    val order: Int,
    /** The structural label resource; null means the field displays its exact wire name. */
    val labelResource: String?,
    val control: DescriptorControl,
    val valueKind: DescriptorValueKind,
    val required: Boolean,
    val default: JsonPrimitive?,
    val options: List<DescriptorOption>,
    val wrappedControl: DescriptorControl?,
    val visibleWhen: List<DescriptorCondition>,
    val maxRows: Int?,
) {
    val wireName: String get() = canonicalPath.substringAfterLast('/')

    val section: DescriptorSection get() = when {
        canonicalPath.startsWith(ARGS_PREFIX) -> DescriptorSection.CallArguments
        canonicalPath.startsWith(ACTION_PREFIX) -> DescriptorSection.Action
        canonicalPath.startsWith(TRIGGER_PREFIX) -> DescriptorSection.Trigger
        else -> DescriptorSection.Root
    }

    /** Strict equality against the current values; a missing value never matches. */
    fun visibleIn(values: Map<String, JsonElement>): Boolean = visibleWhen.all { condition ->
        val current = values[condition.canonicalPath]
        current != null && condition.values.any { it == current }
    }
}

/** The sibling groups whose `order` is contiguous among visible fields. */
enum class DescriptorSection { Root, Trigger, Action, CallArguments }

class AutomationDescriptorCatalog private constructor(val fields: List<FieldDescriptor>) {
    /** The visible fields of [section] under [values], in descriptor order. */
    fun visible(section: DescriptorSection, values: Map<String, JsonElement>): List<FieldDescriptor> =
        fields.filter { it.section == section && it.visibleIn(values) }.sortedBy { it.order }

    /** Fills schema defaults of newly visible non-optional fields without replacing any value. */
    fun withDefaults(
        sections: Set<DescriptorSection>,
        values: Map<String, JsonElement>,
    ): Map<String, JsonElement> {
        var current = values
        do {
            val before = current
            sections.forEach { section ->
                visible(section, current).forEach { field ->
                    val default = field.default
                    if (default != null && field.control != DescriptorControl.Optional &&
                        field.canonicalPath !in current
                    ) {
                        current = current + (field.canonicalPath to default)
                    }
                }
            }
        } while (current != before)
        return current
    }

    companion object {
        const val ASSET = "automation-ui-descriptors.v1.json"

        private val BASE_KEYS = setOf(
            "canonical_path", "order", "label_source", "label_resource", "control", "sensitive",
            "validation", "value_kind", "required",
        )
        private val OPTIONAL_KEYS = setOf(
            "default", "options", "wrapped_control", "visible_when", "max_rows",
            "key_max_utf8_bytes", "scalar_kinds", "string_max_utf8_bytes",
        )

        fun parse(text: String): AutomationDescriptorCatalog {
            val root = Json.parseToJsonElement(text).jsonObject
            require(root.keys == setOf("schema_version", "fields")) { "descriptor root is not closed" }
            require(root.getValue("schema_version").jsonPrimitive.intOrNull == 1) {
                "descriptor schema_version must be 1"
            }
            return AutomationDescriptorCatalog(root.getValue("fields").jsonArray.map(::field))
        }

        private fun field(element: JsonElement): FieldDescriptor {
            val value = element.jsonObject
            require(value.keys.containsAll(BASE_KEYS - "label_resource")) { "descriptor is incomplete" }
            require((value.keys - BASE_KEYS - OPTIONAL_KEYS).isEmpty()) { "descriptor has an unknown key" }
            require(value.string("validation") == "contract") { "descriptor validation is not contract" }
            val labelSource = value.string("label_source")
            val labelResource = value["label_resource"]?.jsonPrimitive?.contentOrNull
            require(
                (labelSource == "resource" && labelResource != null) ||
                    (labelSource == "wire_name" && labelResource == null),
            ) { "descriptor label is inconsistent" }
            return FieldDescriptor(
                canonicalPath = value.string("canonical_path"),
                order = requireNotNull(value.getValue("order").jsonPrimitive.intOrNull),
                labelResource = labelResource,
                control = control(value.string("control")),
                valueKind = DescriptorValueKind.entries.single { it.wire == value.string("value_kind") },
                required = requireNotNull(value.getValue("required").jsonPrimitive.booleanOrNull),
                default = value["default"]?.jsonPrimitive,
                options = value["options"]?.jsonArray?.map { option ->
                    val fields = option.jsonObject
                    val source = fields.string("label_source")
                    val resource = fields["label_resource"]?.jsonPrimitive?.contentOrNull
                    require((source == "resource") == (resource != null)) { "option label is inconsistent" }
                    DescriptorOption(fields.string("value"), resource)
                }.orEmpty(),
                wrappedControl = value["wrapped_control"]?.jsonPrimitive?.content?.let(::control),
                visibleWhen = value["visible_when"]?.jsonArray?.map { condition ->
                    val fields = condition.jsonObject
                    DescriptorCondition(
                        fields.string("canonical_path"),
                        fields.getValue("values").jsonArray.map { it.jsonPrimitive },
                    )
                }.orEmpty(),
                maxRows = value["max_rows"]?.jsonPrimitive?.intOrNull,
            )
        }

        private fun control(wire: String) = DescriptorControl.entries.single { it.wire == wire }

        private fun JsonObject.string(key: String): String =
            requireNotNull(getValue(key).jsonPrimitive.takeIf { it.isString }?.content) { "$key is not a string" }
    }
}

/**
 * One editor action node. Values are keyed by the S-UI-008 template path relative to
 * `/action/<variant>`; recursive slots are structure, never descriptor fields.
 */
data class ActionDraft(
    val values: Map<String, JsonElement>,
    val children: List<ActionDraft> = emptyList(),
    val thenAction: ActionDraft? = null,
    val elseAction: ActionDraft? = null,
    val repeatedAction: ActionDraft? = null,
) {
    val type: String? get() = (values[ACTION_TYPE] as? JsonPrimitive)?.contentOrNull

    companion object {
        fun ofType(type: String) = ActionDraft(mapOf(ACTION_TYPE to JsonPrimitive(type)))
    }
}

/** The editor draft of one Automation definition; root and trigger values use template paths. */
data class AutomationDraft(
    val automationId: String?,
    val expectedRevision: Long?,
    val values: Map<String, JsonElement>,
    val action: ActionDraft,
) {
    companion object {
        fun new() = AutomationDraft(
            automationId = null,
            expectedRevision = null,
            values = mapOf("/enabled" to JsonPrimitive(true)),
            action = ActionDraft.ofType("delay"),
        )
    }
}

/** Converts between the public Automation wire shape and the descriptor-keyed draft. */
object AutomationWire {
    /** The `automation.save` input: only fields visible under the current draft are sent. */
    fun saveInput(catalog: AutomationDescriptorCatalog, draft: AutomationDraft): JsonObject {
        val output = linkedMapOf<String, JsonElement>()
        draft.automationId?.let { output["automation_id"] = JsonPrimitive(it) }
        draft.expectedRevision?.let { output["expected_revision"] = JsonPrimitive(it) }
        val trigger = linkedMapOf<String, JsonElement>()
        listOf(DescriptorSection.Root, DescriptorSection.Trigger).forEach { section ->
            catalog.visible(section, draft.values).forEach { field ->
                val value = draft.values[field.canonicalPath] ?: return@forEach
                if (section == DescriptorSection.Root) {
                    output[field.wireName] = value
                } else {
                    trigger[field.wireName] = value
                }
            }
        }
        output["trigger"] = JsonObject(trigger)
        output["action"] = actionJson(catalog, draft.values, draft.action)
        return JsonObject(output)
    }

    fun actionJson(
        catalog: AutomationDescriptorCatalog,
        rootValues: Map<String, JsonElement>,
        node: ActionDraft,
    ): JsonObject {
        val context = rootValues + node.values
        val output = linkedMapOf<String, Any>()
        listOf(DescriptorSection.Action, DescriptorSection.CallArguments).forEach { section ->
            catalog.visible(section, context).forEach { field ->
                node.values[field.canonicalPath]?.let { value ->
                    place(output, wirePath(field.canonicalPath), value)
                }
            }
        }
        when (node.type) {
            "sequence" -> output["children"] = JsonArray(
                node.children.map { actionJson(catalog, rootValues, it) },
            )
            "conditional" -> {
                node.thenAction?.let { output["then"] = actionJson(catalog, rootValues, it) }
                node.elseAction?.let { output["else"] = actionJson(catalog, rootValues, it) }
            }
            "repeat" -> node.repeatedAction?.let {
                output["action"] = actionJson(catalog, rootValues, it)
            }
        }
        return freeze(output)
    }

    /** The draft of a saved Automation from `automation.get`. */
    fun draftOf(automation: JsonObject): AutomationDraft {
        val values = linkedMapOf<String, JsonElement>()
        values["/name"] = automation.getValue("name")
        values["/enabled"] = automation.getValue("enabled")
        automation.getValue("trigger").jsonObject.forEach { (key, value) ->
            values["$TRIGGER_PREFIX$key"] = value
        }
        return AutomationDraft(
            automationId = automation.getValue("automation_id").jsonPrimitive.content,
            expectedRevision = automation.getValue("revision").jsonPrimitive.content.toLong(),
            values = values,
            action = actionDraftOf(automation.getValue("action").jsonObject),
        )
    }

    private fun actionDraftOf(node: JsonObject): ActionDraft {
        val type = node.getValue("type").jsonPrimitive.content
        val values = linkedMapOf<String, JsonElement>(ACTION_TYPE to JsonPrimitive(type))
        node.forEach { (key, value) ->
            when {
                key == "type" || (type == "sequence" && key == "children") ||
                    (type == "conditional" && (key == "then" || key == "else")) ||
                    (type == "repeat" && key == "action") -> Unit
                type == "call" && key == "tool" -> values["$ACTION_PREFIX$type/tool"] = value
                type == "call" && key == "action" -> values["$ACTION_PREFIX$type/action"] = value
                type == "call" && key == "args" -> flatten(
                    "$ARGS_PREFIX${node.getValue("tool").jsonPrimitive.content}/" +
                        node.getValue("action").jsonPrimitive.content,
                    value,
                    values,
                )
                type == "conditional" && key == "condition" -> value.jsonObject.forEach { (field, leaf) ->
                    values["${ACTION_PREFIX}conditional/condition/$field"] = leaf
                }
                else -> values["$ACTION_PREFIX$type/$key"] = value
            }
        }
        return ActionDraft(
            values = values,
            children = if (type == "sequence") {
                node.getValue("children").jsonArray.map { actionDraftOf(it.jsonObject) }
            } else {
                emptyList()
            },
            thenAction = node["then"]?.takeIf { type == "conditional" }?.let { actionDraftOf(it.jsonObject) },
            elseAction = node["else"]?.takeIf { type == "conditional" }?.let { actionDraftOf(it.jsonObject) },
            repeatedAction = node["action"]?.takeIf { type == "repeat" }?.let { actionDraftOf(it.jsonObject) },
        )
    }

    /** Fixed nested argument objects flatten by exact wire name; the match table stays whole. */
    private fun flatten(prefix: String, value: JsonElement, values: MutableMap<String, JsonElement>) {
        if (value is JsonObject) {
            value.forEach { (key, field) -> flatten("$prefix/$key", field, values) }
        } else {
            values[prefix] = value
        }
    }

    private fun wirePath(canonicalPath: String): List<String> = when {
        canonicalPath == ACTION_TYPE -> listOf("type")
        canonicalPath.startsWith(ARGS_PREFIX) ->
            listOf("args") + canonicalPath.removePrefix(ARGS_PREFIX).split('/').drop(2)
        canonicalPath.startsWith("${ACTION_PREFIX}conditional/condition/") ->
            listOf("condition", canonicalPath.substringAfterLast('/'))
        else -> listOf(canonicalPath.substringAfterLast('/'))
    }

    /** Builds nested argument objects; values are [JsonElement]s or nested mutable maps. */
    private fun place(output: MutableMap<String, Any>, path: List<String>, value: JsonElement) {
        if (path.size == 1) {
            output[path.single()] = value
            return
        }
        @Suppress("UNCHECKED_CAST")
        val nested = output.getOrPut(path.first()) { linkedMapOf<String, Any>() } as MutableMap<String, Any>
        place(nested, path.drop(1), value)
    }

    private fun freeze(output: Map<String, Any>): JsonObject = JsonObject(
        output.mapValues { (_, value) ->
            @Suppress("UNCHECKED_CAST")
            when (value) {
                is JsonElement -> value
                else -> freeze(value as Map<String, Any>)
            }
        },
    )
}

/** The editable text of a scalar leaf; integers stay unparsed text until the Contract validates. */
fun JsonElement?.editorText(): String = when (this) {
    null, JsonNull -> ""
    is JsonPrimitive -> content
    else -> toString()
}

/** A typed integer when the text is one, otherwise the text itself for the Contract to reject. */
fun integerValue(text: String): JsonPrimitive = text.toLongOrNull()?.let(::JsonPrimitive) ?: JsonPrimitive(text)

const val ACTION_PREFIX = "/action/"
const val ACTION_TYPE = "/action/type"
const val TRIGGER_PREFIX = "/trigger/"
const val ARGS_PREFIX = "/action/call/args/"
