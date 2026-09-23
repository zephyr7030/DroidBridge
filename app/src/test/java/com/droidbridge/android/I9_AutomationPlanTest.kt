package com.droidbridge.android

import com.droidbridge.android.product.automation.AutomationPlan
import com.droidbridge.android.product.automation.AutomationPlans
import com.droidbridge.android.product.automation.AutomationSteps
import com.droidbridge.android.product.automation.ElementBy
import com.droidbridge.android.product.automation.PlanKey
import com.droidbridge.android.product.automation.PlanRunAs
import com.droidbridge.android.product.automation.PlanStep
import com.droidbridge.android.product.automation.PlanTrigger
import com.droidbridge.android.product.automation.StepLine
import java.time.DayOfWeek
import java.time.LocalDate
import java.time.LocalTime
import java.time.ZoneId
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class I9_AutomationPlanTest {
    private val zone = ZoneId.of("Asia/Shanghai")
    private val today = LocalDate.of(2026, 9, 22)

    private fun saved(plan: AutomationPlan): JsonObject = JsonObject(
        AutomationPlans.encode(plan, zone, today) + ("automation_id" to JsonPrimitive("id")),
    )

    @Test
    fun everyTemplateStepAndTriggerSurvivesASaveAndReload() {
        val steps = listOf(
            PlanStep.OpenApp("com.example.app"),
            PlanStep.TapElement(ElementBy.Text, "签到", waitSeconds = 20, continueOnFailure = true),
            PlanStep.TapElement(ElementBy.ResourceId, "confirm", longPress = true),
            PlanStep.TypeText("hello"),
            PlanStep.TypeText("hello", target = "搜索", waitSeconds = 5),
            PlanStep.PressKey(PlanKey.Back),
            PlanStep.Wait(3),
            PlanStep.RunCommand("echo hi", PlanRunAs.Root, continueOnFailure = true),
            PlanStep.CopyText("copied"),
        )
        val triggers = listOf(
            PlanTrigger.Daily(LocalTime.of(8, 30)),
            PlanTrigger.Weekly(setOf(DayOfWeek.MONDAY, DayOfWeek.FRIDAY), LocalTime.of(21, 5)),
            PlanTrigger.Every(90),
            PlanTrigger.Once(today.plusDays(1).atTime(7, 0)),
            PlanTrigger.RuntimeStarted,
            PlanTrigger.NetworkChanged(null),
            PlanTrigger.NetworkChanged("wifi"),
        )
        triggers.forEach { trigger ->
            val plan = AutomationPlan("morning", true, trigger, steps)
            val decoded = AutomationPlans.decode(saved(plan))
            if (trigger is PlanTrigger.Once) {
                // A one-off time is stored as an instant and read back in the device zone.
                assertEquals(plan.copy(trigger = decoded!!.trigger), decoded)
            } else {
                assertEquals(plan, decoded)
            }
        }
    }

    @Test
    fun templatesSaveTheShapesTheRuntimeValidates() {
        val plan = AutomationPlan(
            "weekly",
            true,
            PlanTrigger.Weekly(setOf(DayOfWeek.WEDNESDAY, DayOfWeek.MONDAY), LocalTime.of(8, 0)),
            listOf(PlanStep.TapElement(ElementBy.TextContains, "打卡", continueOnFailure = true)),
        )
        val encoded = AutomationPlans.encode(plan, zone, today)
        val trigger = encoded.getValue("trigger").jsonObject
        assertEquals("DTSTART:20260922T080000\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE", trigger.getValue("rrule").jsonPrimitive.content)
        assertEquals("Asia/Shanghai", trigger.getValue("timezone").jsonPrimitive.content)
        val step = encoded.getValue("action").jsonObject.getValue("children").jsonArray.single().jsonObject
        assertEquals(
            Json.parseToJsonElement(
                """{"type":"call","tool":"visual","action":"element",
                   "args":{"operation":"tap","by":"text_contains","value":"打卡","wait_ms":10000},
                   "on_failure":"continue"}""",
            ),
            step,
        )
    }

    @Test
    fun whatNoTemplateRepresentsIsReadOnlyButStillDescribed() {
        val action = Json.parseToJsonElement(
            """{"type":"sequence","children":[
                 {"type":"call","tool":"visual","action":"element",
                  "args":{"operation":"tap","by":"text","value":"签到","wait_ms":10000},"on_failure":"continue"},
                 {"type":"conditional",
                  "condition":{"source":"result","key":"succeeded","operator":"equals","value":false},
                  "then":{"type":"call","tool":"command","action":"run",
                          "args":{"command":"echo retry","run_as":"shell","timeout_ms":5000,
                                  "max_output_bytes":65536,"as_task":false}}}]}""",
        ).jsonObject
        val automation = buildJsonObject {
            put("name", "advanced")
            put("enabled", true)
            put("trigger", buildJsonObject { put("type", "interval"); put("every_ms", 60_000) })
            put("action", action)
        }
        assertNull(AutomationPlans.decode(automation))
        assertEquals(
            listOf(
                StepLine.Step(0, PlanStep.TapElement(ElementBy.Text, "签到", continueOnFailure = true)),
                StepLine.If(0, "result", "succeeded", "equals", "false"),
                // A non-default timeout is not a template command, so it is named by its tool.
                StepLine.Call(1, "command", "run"),
            ),
            AutomationSteps.describe(action),
        )
        assertNull(
            AutomationPlans.decodeTrigger(
                buildJsonObject {
                    put("type", "rrule")
                    put("rrule", "DTSTART:20260922T080000\nRRULE:FREQ=MONTHLY")
                    put("timezone", "Asia/Shanghai")
                },
            ),
        )
    }
}
