package com.droidbridge.android.execution.android

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull

internal interface ExactAlarmAccess {
    fun canScheduleExactAlarms(): Boolean
    fun setExactAndAllowWhileIdle(triggerAtMillis: Long)
    fun cancel()
}

/**
 * The one S-LIFE-003 APK wake: an RTC_WAKEUP exact alarm whose immutable private PendingIntent
 * targets [receiver]. Re-arming replaces the same PendingIntent, so at most one alarm exists.
 */
internal class AndroidExactAlarmAccess(
    private val context: Context,
    private val receiver: Class<*>,
) : ExactAlarmAccess {
    private val alarms = context.getSystemService(AlarmManager::class.java)

    override fun canScheduleExactAlarms(): Boolean = alarms.canScheduleExactAlarms()

    override fun setExactAndAllowWhileIdle(triggerAtMillis: Long) {
        alarms.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, triggerAtMillis, operation())
    }

    override fun cancel() {
        alarms.cancel(operation())
    }

    private fun operation(): PendingIntent = PendingIntent.getBroadcast(
        context,
        REQUEST_CODE,
        Intent(context, receiver).setPackage(context.packageName),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    private companion object {
        const val REQUEST_CODE = 0
    }
}

/**
 * Executes the `AlarmSchedule`/`AlarmCancel` primitives for the Rust scheduler's wake
 * projection. It stores no due: every arm carries the canonical earliest due.
 */
internal class ExactAlarmAdapter(
    private val access: ExactAlarmAccess,
    private val validatesFence: (String, Long, String) -> Boolean,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException("STALE_AUTHORITY")
        }
        if (request.descriptors.isNotEmpty()) throw AndroidExecutionException("INVALID_ARGUMENT")
        if (request.payload.size > MAX_PAYLOAD_BYTES) throw AndroidExecutionException("RESOURCE_LIMIT")
        return when (request.primitive) {
            AndroidPrimitive.AlarmSchedule -> {
                val due = decodeSchedule(request.payload)
                if (!access.canScheduleExactAlarms()) {
                    throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
                }
                platform { access.setExactAndAllowWhileIdle(due) }
                AndroidExecutionResult(SCHEDULED)
            }
            AndroidPrimitive.AlarmCancel -> {
                if (decode(request.payload).isNotEmpty()) {
                    throw AndroidExecutionException("INVALID_ARGUMENT")
                }
                platform { access.cancel() }
                AndroidExecutionResult(CANCELLED)
            }
            else -> throw AndroidExecutionException("UNSUPPORTED")
        }
    }

    private fun platform(operation: () -> Unit) {
        try {
            operation()
        } catch (_: SecurityException) {
            throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
        } catch (error: RuntimeException) {
            if (error is AndroidExecutionException) throw error
            throw AndroidExecutionException("IO_ERROR")
        }
    }

    private fun decodeSchedule(payload: ByteArray): Long {
        val value = decode(payload)
        if (value.keys != SCHEDULE_KEYS) throw AndroidExecutionException("INVALID_ARGUMENT")
        val due = value["due_unix_millis"]?.jsonPrimitive
            ?.takeIf { !it.isString }
            ?.longOrNull
            ?: throw AndroidExecutionException("INVALID_ARGUMENT")
        if (due <= 0) throw AndroidExecutionException("INVALID_ARGUMENT")
        return due
    }

    private fun decode(payload: ByteArray): JsonObject = try {
        Json.parseToJsonElement(payload.decodeToString(throwOnInvalidSequence = true)).jsonObject
    } catch (_: RuntimeException) {
        throw AndroidExecutionException("INVALID_ARGUMENT")
    }

    private companion object {
        const val MAX_PAYLOAD_BYTES = 4_096
        val SCHEDULE_KEYS = setOf("due_unix_millis")
        val SCHEDULED = "{\"scheduled\":true}".encodeToByteArray()
        val CANCELLED = "{\"cancelled\":true}".encodeToByteArray()
    }
}
