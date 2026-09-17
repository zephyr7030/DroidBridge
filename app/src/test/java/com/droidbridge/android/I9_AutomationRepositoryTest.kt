package com.droidbridge.android

import com.droidbridge.android.product.automation.AutomationRepository
import com.droidbridge.android.product.automation.AutomationResult
import com.droidbridge.android.product.automation.BulkDeleteOutcome
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Test

class I9_AutomationRepositoryTest {
    @Test
    fun I9_G06_deleteAllUsesOneSnapshotOrderAndNeverRetriesConflicts() = runBlocking {
        val requests = mutableListOf<JsonObject>()
        val repository = AutomationRepository(
            submit = { bytes ->
                val request = Json.parseToJsonElement(bytes.decodeToString()).jsonObject
                requests += request
                val payload = request.getValue("payload").jsonObject
                val input = payload.getValue("input").jsonObject
                when (payload.getValue("action").jsonPrimitive.content) {
                    "list" -> success(
                        """{"automations":[
                          {"automation_id":"a","name":"A","enabled":true,"revision":3,"updated_at":"2026-09-14T08:00:00.000Z"},
                          {"automation_id":"c","name":"C","enabled":true,"revision":1,"updated_at":"2026-09-14T09:00:00.000Z"},
                          {"automation_id":"b","name":"B","enabled":false,"revision":7,"updated_at":"2026-09-14T09:00:00.000Z"}
                        ]}""",
                    )
                    "delete" -> if (input.getValue("automation_id").jsonPrimitive.content == "b") {
                        """{"protocol_version":1,"outcome":"error","error":{"code":"REVISION_CONFLICT","operation":"automation.delete","retryable":false}}"""
                            .encodeToByteArray()
                    } else {
                        success("""{"automation_id":"x","deleted":true,"previous_revision":1}""")
                    }
                    else -> error("unexpected action")
                }
            },
        )

        assertEquals(
            AutomationResult.Success(BulkDeleteOutcome(deleted = 2, failed = 1)),
            repository.deleteAll(),
        )
        assertEquals(500, requests.first().input()["limit"]!!.jsonPrimitive.content.toInt())
        val deletes = requests.drop(1).map { request ->
            request.input().let { it.getValue("automation_id").jsonPrimitive.content to it.getValue("expected_revision").jsonPrimitive.content.toLong() }
        }
        // (updated_at desc, automation_id desc), each at its snapshot revision, conflict not retried.
        assertEquals(listOf("c" to 1L, "b" to 7L, "a" to 3L), deletes)
        // Every mutation is its own ordinary retained request.
        assertEquals(requests.size, requests.map { it.getValue("request_id").jsonPrimitive.content }.toSet().size)
    }

    @Test
    fun I9_G06_runtimeErrorsAreReturnedVerbatimAndTransportLossIsExplicit() = runBlocking {
        val rejected = AutomationRepository(
            submit = {
                """{"protocol_version":1,"outcome":"error","error":{"code":"INVALID_ARGUMENT","operation":"automation.save","retryable":false,"message":"trigger.at is in the past"}}"""
                    .encodeToByteArray()
            },
        ).save(JsonObject(emptyMap()))
        val failure = rejected as AutomationResult.Failure
        assertEquals("INVALID_ARGUMENT", failure.error.code)
        assertEquals("trigger.at is in the past", failure.error.message)

        val lost = AutomationRepository(submit = { error("binder died") }).list()
        assertEquals("RUNTIME_UNAVAILABLE", (lost as AutomationResult.Failure).error.code)
    }

    private fun JsonObject.input(): JsonObject =
        getValue("payload").jsonObject.getValue("input").jsonObject

    private fun success(result: String): ByteArray =
        """{"protocol_version":1,"outcome":"success","result":$result}""".encodeToByteArray()
}
