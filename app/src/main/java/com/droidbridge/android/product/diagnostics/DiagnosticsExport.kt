package com.droidbridge.android.product.diagnostics

import java.io.File
import java.io.IOException
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.time.format.DateTimeParseException
import java.time.temporal.ChronoUnit
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

enum class FaultFileStatus(val wire: String) { Ok("ok"), Missing("missing"), Corrupt("corrupt"), Unreadable("unreadable") }

/** One S-SEC-005 role file as read by the default process; `records` exists only for `Ok`. */
data class FaultFileRead(val status: FaultFileStatus, val records: JsonArray? = null)

/**
 * The default-process S-SEC-005 export. It reads the bounded App-owned fault files directly with JDK
 * file APIs, so prior-instance faults export even when Runtime binding or bootstrap failed, and it
 * never guesses a live fact the S-UI-017 snapshot did not return.
 */
object DiagnosticsExport {
    const val EXPORT_LIMIT_BYTES = 1_048_576
    val ROLES = listOf("runtime", "host", "supervisor", "maintenance")
    private const val FILE_LIMIT_BYTES = 65_536
    private const val RECORD_LIMIT = 64
    private val requiredRecordKeys =
        setOf("record_id", "at", "component", "code", "phase", "product_version", "boot_id", "repeat_count")
    private val optionalRecordKeys = setOf("runtime_instance_id", "execution_id", "exit_code", "signal")
    private val uuid = Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")
    private val millis = DateTimeFormatter.ofPattern("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'").withZone(ZoneOffset.UTC)
    private val fileStamp = DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss").withZone(ZoneOffset.UTC)

    fun readFaultFiles(canonicalBase: File): Map<String, FaultFileRead> =
        ROLES.associateWith { role -> readFaultFile(File(File(canonicalBase, "diagnostics"), "$role.json")) }

    fun readFaultFile(file: File): FaultFileRead {
        if (!file.exists()) return FaultFileRead(FaultFileStatus.Missing)
        val bytes = try {
            if (file.length() > FILE_LIMIT_BYTES) return FaultFileRead(FaultFileStatus.Corrupt)
            file.readBytes()
        } catch (_: IOException) {
            return FaultFileRead(FaultFileStatus.Unreadable)
        } catch (_: SecurityException) {
            return FaultFileRead(FaultFileStatus.Unreadable)
        }
        return runCatching {
            val value = Json.parseToJsonElement(bytes.decodeToString()).jsonObject
            require(value.keys == setOf("schema_version", "records"))
            require((value.getValue("schema_version") as JsonPrimitive).let { !it.isString && it.content == "1" })
            val records = value.getValue("records") as JsonArray
            require(records.size <= RECORD_LIMIT && records.all(::validRecord))
            FaultFileRead(FaultFileStatus.Ok, records)
        }.getOrElse { FaultFileRead(FaultFileStatus.Corrupt) }
    }

    fun fileName(now: Instant): String = "droidbridge-diagnostics-${fileStamp.format(now)}.json"

    /** Builds the export; the live snapshot is included exactly when it carries a completed status read. */
    fun build(
        generatedAt: Instant,
        productVersions: JsonObject,
        liveSnapshotReply: String?,
        faultFiles: Map<String, FaultFileRead>,
        releaseIdentifiers: JsonObject,
    ): String {
        val liveSnapshot = liveSnapshotReply?.let { reply ->
            runCatching { Json.parseToJsonElement(reply).jsonObject }.getOrNull()
        }?.takeIf { snapshot ->
            (snapshot["schema_version"] as? JsonPrimitive)?.let { !it.isString && it.content == "1" } == true &&
                snapshot["status"] is JsonObject
        }
        val encoded = buildJsonObject {
            put("schema_version", 1)
            put("generated_at", millis.format(generatedAt.truncatedTo(ChronoUnit.MILLIS)))
            put("product_versions", productVersions)
            put("live_status", if (liveSnapshot != null) "available" else "unavailable")
            liveSnapshot?.let { put("live_snapshot", it) }
            put("fault_files", buildJsonObject {
                ROLES.forEach { role ->
                    val read = faultFiles[role] ?: FaultFileRead(FaultFileStatus.Missing)
                    put(role, buildJsonObject {
                        put("status", read.status.wire)
                        read.records?.let { put("records", it) }
                    })
                }
            })
            // The UpdateManager maintenance ring is owned by I12 and has no entries before it exists.
            put("maintenance_ring", buildJsonArray { })
            put("release_identifiers", releaseIdentifiers)
        }.toString()
        check(encoded.encodeToByteArray().size <= EXPORT_LIMIT_BYTES) { "diagnostic export exceeds its bound" }
        return encoded
    }

    private fun validRecord(element: JsonElement): Boolean {
        val record = element as? JsonObject ?: return false
        if (!record.keys.containsAll(requiredRecordKeys) || !(requiredRecordKeys + optionalRecordKeys).containsAll(record.keys)) {
            return false
        }
        fun string(key: String) = (record[key] as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content
        fun boundedAscii(key: String) = string(key)?.let { it.isNotEmpty() && it.length <= 64 && it.all { c -> c.code < 128 } } == true
        fun uuidField(key: String, required: Boolean) =
            if (record.containsKey(key)) string(key)?.let(uuid::matches) == true else !required
        fun intField(key: String) =
            !record.containsKey(key) || (record[key] as? JsonPrimitive)?.takeUnless(JsonPrimitive::isString)?.intOrNull != null
        val at = string("at") ?: return false
        val canonicalAt = try {
            millis.format(Instant.parse(at)) == at
        } catch (_: DateTimeParseException) {
            false
        }
        val repeat = (record["repeat_count"] as? JsonPrimitive)?.takeUnless(JsonPrimitive::isString)?.longOrNull
        return canonicalAt &&
            uuidField("record_id", required = true) &&
            uuidField("boot_id", required = true) &&
            uuidField("runtime_instance_id", required = false) &&
            uuidField("execution_id", required = false) &&
            listOf("component", "code", "phase", "product_version").all(::boundedAscii) &&
            intField("exit_code") && intField("signal") &&
            repeat != null && repeat in 1..4_294_967_295L
    }
}
