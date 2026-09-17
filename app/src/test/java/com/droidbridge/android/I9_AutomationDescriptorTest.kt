package com.droidbridge.android

import com.droidbridge.android.product.automation.ActionDraft
import com.droidbridge.android.product.automation.AutomationDescriptorCatalog
import com.droidbridge.android.product.automation.AutomationDraft
import com.droidbridge.android.product.automation.AutomationWire
import com.droidbridge.android.product.automation.DescriptorControl
import com.droidbridge.android.product.automation.DescriptorSection
import java.io.File
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class I9_AutomationDescriptorTest {
    private val catalog = AutomationDescriptorCatalog.parse(
        File("../tools/fixtures/contract/automation-ui-descriptors.v1.json").readText(),
    )

    @Test
    fun I9_G06_triggerTypeAloneSelectsTheVisibleTriggerFields() {
        fun visiblePaths(type: String) = catalog
            .visible(DescriptorSection.Trigger, mapOf("/trigger/type" to JsonPrimitive(type)))
            .map { it.canonicalPath }

        assertEquals(listOf("/trigger/type", "/trigger/at"), visiblePaths("at"))
        assertEquals(listOf("/trigger/type", "/trigger/every_ms"), visiblePaths("interval"))
        assertEquals(listOf("/trigger/type", "/trigger/rrule", "/trigger/timezone"), visiblePaths("rrule"))
        assertEquals(listOf("/trigger/type", "/trigger/name", "/trigger/match"), visiblePaths("event"))
        assertEquals(
            DescriptorControl.KeyScalarTable,
            catalog.visible(DescriptorSection.Trigger, mapOf("/trigger/type" to JsonPrimitive("event"))).last().control,
        )
    }

    @Test
    fun I9_G06_callArgumentsFollowTheSelectedToolActionAndDiscriminator() {
        val tcp = mapOf(
            "/action/type" to JsonPrimitive("call"),
            "/action/call/tool" to JsonPrimitive("network"),
            "/action/call/action" to JsonPrimitive("diagnose"),
            "/action/call/args/network/diagnose/test" to JsonPrimitive("tcp"),
        )
        val arguments = catalog.visible(DescriptorSection.CallArguments, tcp)
        assertEquals(
            listOf("test", "host", "port", "timeout_ms"),
            arguments.map { it.wireName },
        )
        // Wire-name leaves carry no invented label; the action selector is tool-specific.
        assertTrue(arguments.all { it.labelResource == null })
        assertEquals(
            listOf("diagnose"),
            catalog.visible(DescriptorSection.Action, tcp)
                .single { it.canonicalPath == "/action/call/action" }
                .options.map { it.value },
        )
        // A dependent field appears only for its own discriminator value.
        val dnsName = catalog.fields.single { it.canonicalPath == "/action/call/args/network/diagnose/name" }
        assertFalse(dnsName.visibleIn(tcp))
        assertTrue(dnsName.visibleIn(tcp + ("/action/call/args/network/diagnose/test" to JsonPrimitive("dns"))))
    }

    @Test
    fun I9_G06_savedDefinitionRoundTripsThroughTheDescriptorDraft() {
        val saved = Json.parseToJsonElement(
            """
            {"automation_id":"99400000-0000-4000-8000-000000000010","name":"wifi maintenance",
             "enabled":false,"revision":4,"created_at":"2026-09-14T08:00:00.000Z",
             "updated_at":"2026-09-14T08:00:00.000Z","state":{},
             "trigger":{"type":"event","name":"network.default_changed",
                        "match":{"transport":"wifi","host_generation":3}},
             "action":{"type":"sequence","children":[
               {"type":"call","tool":"command","action":"run","args":{"command":"id","run_as":"app",
                "cwd":"/data/local/tmp","timeout_ms":30000,"max_output_bytes":65536,"as_task":false}},
               {"type":"conditional",
                "condition":{"source":"state","key":"reachable","operator":"equals","value":1},
                "then":{"type":"call","tool":"network","action":"diagnose",
                        "args":{"test":"tcp","host":"example.com","port":443,"timeout_ms":5000}},
                "else":{"type":"set_state","key":"reachable","value":null}},
               {"type":"repeat","count":2,"delay_ms":0,"action":{"type":"delay","duration_ms":1}}
             ]}}
            """.trimIndent(),
        ).jsonObject

        val draft = AutomationWire.draftOf(saved)
        val input = AutomationWire.saveInput(catalog, draft)

        assertEquals(JsonPrimitive("99400000-0000-4000-8000-000000000010"), input["automation_id"])
        assertEquals(JsonPrimitive(4L), input["expected_revision"])
        val expected = saved.filterKeys { it in setOf("name", "enabled", "trigger", "action") }
        assertEquals(expected, input.filterKeys { it in expected.keys })
    }

    @Test
    fun I9_G06_hiddenAndExcludedFieldsAreNeverSerialized() {
        var draft = AutomationDraft.new().copy(
            values = mapOf(
                "/name" to JsonPrimitive("probe"),
                "/enabled" to JsonPrimitive(true),
                "/trigger/type" to JsonPrimitive("interval"),
                "/trigger/every_ms" to JsonPrimitive(60_000L),
                // Left over from an earlier trigger type: hidden, so not sent.
                "/trigger/at" to JsonPrimitive("2026-09-15T08:00:00Z"),
            ),
            action = ActionDraft(
                mapOf(
                    "/action/type" to JsonPrimitive("call"),
                    "/action/call/tool" to JsonPrimitive("command"),
                    "/action/call/action" to JsonPrimitive("run"),
                    "/action/call/args/command/run/command" to JsonPrimitive("id"),
                    "/action/call/args/command/run/run_as" to JsonPrimitive("shell"),
                ),
            ),
        )
        val defaults: Map<String, JsonElement> = catalog.withDefaults(
            setOf(DescriptorSection.Action, DescriptorSection.CallArguments),
            draft.values + draft.action.values,
        )
        draft = draft.copy(action = draft.action.copy(values = defaults.filterKeys { it.startsWith("/action/") }))
        val input = AutomationWire.saveInput(catalog, draft)

        assertEquals(
            Json.parseToJsonElement("""{"type":"interval","every_ms":60000}"""),
            input["trigger"],
        )
        val args = (input["action"] as JsonObject)["args"] as JsonObject
        // Schema defaults fill in; the optional cwd/stdin wrappers start excluded.
        assertEquals(setOf("command", "run_as", "timeout_ms", "max_output_bytes", "as_task"), args.keys)
        assertEquals(JsonPrimitive(30_000L), args["timeout_ms"])
    }
}
