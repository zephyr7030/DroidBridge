package com.droidbridge.android

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class I1_KotlinEnvelopeFixtureTest {
    @Test
    fun generatedFixturesMatchTheStrictV1Envelope() {
        val text = checkNotNull(javaClass.classLoader?.getResourceAsStream(
            "contract/kotlin-envelope-fixtures.v1.json",
        )).bufferedReader(Charsets.UTF_8).use { it.readText() }
        val root = Json.parseToJsonElement(text).jsonObject
        assertEquals(1, root.getValue("schema_version").jsonPrimitive.content.toInt())
        val fixtures = root.getValue("fixtures").jsonArray
        assertEquals(3, fixtures.size)

        fixtures.forEach { fixtureElement ->
            val fixture = fixtureElement.jsonObject
            val envelope = fixture.getValue("json").jsonObject
            assertEquals(1, envelope.getValue("protocol_version").jsonPrimitive.content.toInt())
            val name = fixture.getValue("name").jsonPrimitive.content
            when (name) {
                "context_status_request" -> {
                    assertEquals(setOf("protocol_version", "request_id", "payload"), envelope.keys)
                    val payload = envelope.getValue("payload").jsonObject
                    assertEquals(setOf("tool", "action", "input"), payload.keys)
                    assertEquals("context", payload.getValue("tool").jsonPrimitive.content)
                    assertEquals("status", payload.getValue("action").jsonPrimitive.content)
                }
                "success_response" -> {
                    assertEquals("success", envelope.getValue("outcome").jsonPrimitive.content)
                    assertTrue("result" in envelope)
                    assertFalse("error" in envelope)
                }
                "error_response" -> {
                    assertEquals("error", envelope.getValue("outcome").jsonPrimitive.content)
                    assertTrue("error" in envelope)
                    assertFalse("result" in envelope)
                }
                else -> error("Unknown generated fixture: $name")
            }
        }
    }
}
