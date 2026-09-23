package com.droidbridge.android

import com.droidbridge.android.product.runtime.PublicResult
import com.droidbridge.android.product.about.ProductInfo
import com.droidbridge.android.product.diagnostics.DiagnosticsExport
import com.droidbridge.android.product.diagnostics.FaultFileStatus
import com.droidbridge.android.product.home.AgentConnectionSummary
import com.droidbridge.android.product.home.ConnectedAgent
import com.droidbridge.android.product.home.HomeMcpRow
import com.droidbridge.android.product.home.HomeProjection
import com.droidbridge.android.product.home.HomeUsability
import com.droidbridge.android.product.maintenance.MaintenanceBlocker
import com.droidbridge.android.product.maintenance.MaintenanceReplies
import com.droidbridge.android.product.tasks.TaskFilter
import com.droidbridge.android.product.tasks.TaskPresentation
import com.droidbridge.android.product.tasks.TaskRepository
import com.droidbridge.android.product.tasks.TaskSnapshot
import java.io.File
import java.nio.file.Files
import java.time.Instant
import java.time.ZoneId
import java.util.Locale
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class I11_ProductDataTest {
    @Test
    fun I11_G02_taskFiltersMapExactlyToCanonicalStateSets() = runBlocking {
        assertEquals(listOf("created", "queued", "running"), TaskFilter.Active.states)
        assertEquals(listOf("completed", "failed", "cancelled", "interrupted"), TaskFilter.Completed.states)

        val sent = mutableListOf<String>()
        val repository = TaskRepository(submit = { envelope ->
            sent += envelope.decodeToString()
            """{"protocol_version":1,"request_id":"r","outcome":"success","result":{"tasks":[
                {"task_id":"t1","state":"running","tool":"command","action":"run","created_at":"2026-09-15T08:00:00.000Z","started_at":"2026-09-15T08:00:01.000Z"}
            ]}}""".encodeToByteArray()
        }, requestIds = { "99500000-0000-4000-8000-000000000001" })

        val listed = repository.list(TaskFilter.Completed) as PublicResult.Success
        assertEquals("t1", listed.value.single().taskId)
        val ended = Json.parseToJsonElement(sent[0]).jsonObject.getValue("payload").jsonObject.getValue("input").jsonObject
        assertEquals(TaskFilter.Completed.states, ended.getValue("states").jsonArray.map { it.jsonPrimitive.content })

        repository.list(TaskFilter.Active, limit = 500)
        val input = Json.parseToJsonElement(sent[1]).jsonObject.getValue("payload").jsonObject.getValue("input").jsonObject
        assertEquals(listOf("created", "queued", "running"), input.getValue("states").jsonArray.map { it.jsonPrimitive.content })
        assertEquals(500, input.getValue("limit").jsonPrimitive.content.toInt())
    }

    @Test
    fun I11_G02_cancelIsVisibleExactlyForActiveStatesWithoutARecordedRequest() {
        for (state in listOf("created", "queued", "running")) {
            assertTrue(state, TaskPresentation.cancellable(snapshot(state, cancelRequested = false)))
            assertFalse(state, TaskPresentation.cancellable(snapshot(state, cancelRequested = true)))
        }
        for (state in listOf("completed", "failed", "cancelled", "interrupted")) {
            assertFalse(state, TaskPresentation.cancellable(snapshot(state, cancelRequested = false)))
        }
    }

    @Test
    fun I11_G05_taskDetailValuesAreTheOwnerOutputsVerbatim() = runBlocking {
        val repository = TaskRepository(submit = {
            """{"protocol_version":1,"request_id":"r","outcome":"success","result":{
                "task_id":"t2","state":"completed","tool":"command","action":"run","created_at":"2026-09-15T08:00:00.000Z",
                "cancel_requested":false,"execution_class":"app",
                "result":{"exit_code":0,"stdout_ref":"dbref:stdout:a","image_ref":"dbref:image:b","stderr":"","data_ref":null}
            }}""".encodeToByteArray()
        })
        val fetched = (repository.get("t2") as PublicResult.Success).value
        assertEquals("app", fetched.executionClass)
        assertEquals(listOf("dbref:stdout:a", "dbref:image:b"), TaskPresentation.outputRefs(fetched.result))
        assertEquals(
            "{\n  \"exit_code\": 0,\n  \"stdout_ref\": \"dbref:stdout:a\",\n  \"image_ref\": \"dbref:image:b\",\n  \"stderr\": \"\",\n  \"data_ref\": null\n}",
            TaskPresentation.prettyJson(requireNotNull(fetched.result)),
        )
        assertEquals("{}", TaskPresentation.prettyJson(buildJsonObject { }))
        // The zone conversion and medium localized style are asserted; the separator before the day
        // period is JDK locale data (a plain, no-break or narrow no-break space), so it is normalized.
        assertEquals(
            "Sep 15, 2026, 4:00:00 PM",
            TaskPresentation.formatInstant("2026-09-15T08:00:00.000Z", ZoneId.of("Asia/Shanghai"), Locale.US)
                .replace(' ', ' ')
                .replace(' ', ' '),
        )

        // A Runtime public error is carried as its code; nothing is fabricated.
        val failing = TaskRepository(submit = {
            """{"protocol_version":1,"request_id":"r","outcome":"error","error":{"code":"NOT_FOUND","operation":"task_control.get","retryable":false}}""".encodeToByteArray()
        })
        assertEquals("NOT_FOUND", (failing.get("t9") as PublicResult.Failure).error.code)
        val unavailable = TaskRepository(submit = { error("unbound") })
        assertEquals("RUNTIME_UNAVAILABLE", (unavailable.cancel("t9") as PublicResult.Failure).error.code)
    }

    @Test
    fun I11_G06_homeUpdateSlotDefaultsToNoNewerVersionAndOnlyObservedMismatch() {
        val compatible = HomeProjection.updateSlot(mapOf("protocol" to "compatible", "store_schema" to "unknown"))
        assertFalse(compatible.newerVersionAvailable)
        assertFalse(compatible.visible)
        val mismatch = HomeProjection.updateSlot(mapOf("protocol" to "incompatible", "store_schema" to "compatible"))
        assertFalse(mismatch.newerVersionAvailable)
        assertTrue(mismatch.componentMismatch && mismatch.visible)
        assertFalse(HomeProjection.updateSlot(emptyMap()).visible)

        assertEquals("12", HomeProjection.countLabel(12, HomeProjection.TASK_PAGE_LIMIT))
        assertEquals("499", HomeProjection.countLabel(499, 500))
        assertEquals("500+", HomeProjection.countLabel(500, 500))
    }

    @Test
    fun the_one_agent_connection_row_names_every_connected_agent() {
        // Local MCP and the ChatGPT tunnel are peers under the one entry; each connected one is named.
        assertEquals(
            AgentConnectionSummary(listOf(ConnectedAgent.LocalMcp), unreadable = false),
            HomeProjection.agentConnections(HomeMcpRow.Running, false, false),
        )
        assertEquals(
            AgentConnectionSummary(listOf(ConnectedAgent.ChatGpt), unreadable = false),
            HomeProjection.agentConnections(HomeMcpRow.Off, true, false),
        )
        assertEquals(
            AgentConnectionSummary(listOf(ConnectedAgent.LocalMcp, ConnectedAgent.ChatGpt), unreadable = false),
            HomeProjection.agentConnections(HomeMcpRow.Running, true, false),
        )
        // Enabled but not running is not a live connection.
        assertEquals(AgentConnectionSummary(emptyList(), unreadable = false), HomeProjection.agentConnections(HomeMcpRow.EnabledNotRunning, false, false))
        // A failed read hides nothing that was read, and only states unreadable when nothing is known up.
        assertEquals(AgentConnectionSummary(listOf(ConnectedAgent.ChatGpt), unreadable = false), HomeProjection.agentConnections(HomeMcpRow.Off, true, true))
        assertEquals(AgentConnectionSummary(emptyList(), unreadable = true), HomeProjection.agentConnections(HomeMcpRow.Off, false, true))
    }

    @Test
    fun homeReadsAsUsableOnlyWhenAnAgentCanUseThePhoneNow() {
        val connected = AgentConnectionSummary(listOf(ConnectedAgent.LocalMcp), unreadable = false)
        val none = AgentConnectionSummary(emptyList(), unreadable = false)
        val unread = AgentConnectionSummary(emptyList(), unreadable = true)
        assertEquals(HomeUsability.Usable, HomeProjection.usability(connected, attention = 0, checking = false))
        // A running Runtime that no agent reaches is not usable, whatever else is set up.
        assertEquals(HomeUsability.NoAgentConnected, HomeProjection.usability(none, attention = 3, checking = true))
        assertEquals(HomeUsability.NeedsAttention, HomeProjection.usability(connected, attention = 1, checking = true))
        // Nothing waits on the user, but a fact is still being read: not yet usable.
        assertEquals(HomeUsability.Checking, HomeProjection.usability(connected, attention = 0, checking = true))
        assertEquals(HomeUsability.Checking, HomeProjection.usability(unread, attention = 0, checking = false))
    }

    @Test
    fun I11_G07_diagnosticsExportCarriesPriorInstanceFaultsWithoutALiveRuntime() {
        val base = Files.createTempDirectory("droidbridge-i11-diag").toFile()
        try {
            val diagnostics = File(base, "diagnostics").apply { mkdirs() }
            File(diagnostics, "host.json").writeText(
                """{"schema_version":1,"records":[{"record_id":"99600000-0000-4000-8000-000000000001","at":"2026-09-14T08:00:00.000Z",""" +
                    """"component":"host_controller","code":"IO_ERROR","phase":"host_start","product_version":"0.1.0",""" +
                    """"boot_id":"99600000-0000-4000-8000-000000000002","runtime_instance_id":"99600000-0000-4000-8000-000000000003","repeat_count":2}]}""",
            )
            File(diagnostics, "runtime.json").writeText("""{"schema_version":1,"records":[{"record_id":"bad"}]}""")
            File(diagnostics, "maintenance.json").writeText("""{"schema_version":1,"records":[]}""")

            val files = DiagnosticsExport.readFaultFiles(base)
            assertEquals(FaultFileStatus.Ok, files.getValue("host").status)
            assertEquals(FaultFileStatus.Corrupt, files.getValue("runtime").status)
            assertEquals(FaultFileStatus.Missing, files.getValue("supervisor").status)
            assertEquals(FaultFileStatus.Ok, files.getValue("maintenance").status)

            val now = Instant.parse("2026-09-15T08:09:10.123456Z")
            for (live in listOf(null, """{"schema_version":1,"session":{"started":false,"start_failure":"IO_ERROR"}}""", "not json")) {
                val export = Json.parseToJsonElement(
                    DiagnosticsExport.build(now, buildJsonObject { put("apk_version_name", "0.1.0") }, live, files, buildJsonObject { put("application_id", "x") }),
                ).jsonObject
                assertEquals("unavailable", export.getValue("live_status").jsonPrimitive.content)
                assertFalse(export.containsKey("live_snapshot"))
                assertEquals("2026-09-15T08:09:10.123Z", export.getValue("generated_at").jsonPrimitive.content)
                val host = export.getValue("fault_files").jsonObject.getValue("host").jsonObject
                assertEquals("ok", host.getValue("status").jsonPrimitive.content)
                assertEquals("IO_ERROR", host.getValue("records").jsonArray.single().jsonObject.getValue("code").jsonPrimitive.content)
                assertFalse(export.getValue("fault_files").jsonObject.getValue("runtime").jsonObject.containsKey("records"))
            }
            val available = Json.parseToJsonElement(
                DiagnosticsExport.build(now, buildJsonObject { }, """{"schema_version":1,"session":{"started":true,"host":"apk_runtime"},"status":{"runtime":{}}}""", files, buildJsonObject { }),
            ).jsonObject
            assertEquals("available", available.getValue("live_status").jsonPrimitive.content)
            assertTrue(available.getValue("live_snapshot").jsonObject.containsKey("status"))
            assertEquals("droidbridge-diagnostics-20260915-080910.json", DiagnosticsExport.fileName(now))
        } finally {
            base.deleteRecursively()
        }
    }

    @Test
    fun I11_G05_licensesAndMaintenanceRepliesComeFromTheirOwners() {
        val inventory = File("../tools/third-party-direct.tsv").readText()
        val entries = ProductInfo.licenses(inventory)
        assertTrue(entries.isNotEmpty())
        assertTrue(entries.none { it.name.contains("WinFlexBison") })
        assertEquals(entries.sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.name }), entries)
        assertEquals("1.10.6 | BSD-3-Clause", entries.single { it.name == "libpcap" }.supportingText)
        assertNull(ProductInfo.repositoryUrl("UNCONFIGURED", "droidbridge"))
        assertEquals("https://github.com/o/r", ProductInfo.repositoryUrl("o", "r"))

        val state = requireNotNull(MaintenanceReplies.state("""{"schema_version":1,"blocker":"store_corrupt","cleanup":"verified"}"""))
        assertEquals(MaintenanceBlocker.StoreCorrupt, state.blocker)
        assertTrue(state.recoveryRequired && state.resetAvailable)
        assertFalse(requireNotNull(MaintenanceReplies.state("""{"schema_version":1,"blocker":"owner_corrupt","cleanup":"unverified"}""")).resetAvailable)
        assertNull(MaintenanceReplies.state("""{"schema_version":1,"error":"IO_ERROR"}"""))
        assertTrue(MaintenanceReplies.resetSucceeded("""{"schema_version":1,"reset":true}"""))
        assertFalse(MaintenanceReplies.resetSucceeded("""{"schema_version":1,"error":"HOST_TRANSITION_PENDING"}"""))
    }

    private fun snapshot(state: String, cancelRequested: Boolean) = TaskSnapshot(
        taskId = "t", state = state, tool = "command", action = "run", createdAt = "2026-09-15T08:00:00.000Z",
        startedAt = null, endedAt = null, executionClass = null, cancelRequested = cancelRequested,
        result = JsonPrimitive("x").takeIf { false }, error = null,
    )
}
