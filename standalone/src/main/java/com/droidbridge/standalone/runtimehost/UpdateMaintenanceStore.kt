package com.droidbridge.standalone.runtimehost

import com.droidbridge.standalone.product.update.validUpdateCode
import com.droidbridge.standalone.product.update.validUpdateStage
import java.io.File
import java.io.FileOutputStream
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.nio.file.StandardOpenOption
import java.util.UUID
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

internal enum class MaintenancePhase(val wire: String) {
    Prepared("prepared"),
    ApkInstalling("apk_installing"),
}

internal data class UpdateInstallFailure(val code: String, val stage: String, val atMillis: Long) {
    fun validate() {
        require(validUpdateCode(code) && validUpdateStage(stage) && atMillis >= 0)
    }
}

internal data class UpdateInstallAttempt(
    val id: String,
    val sessionId: Int,
    val terminalCallbackSeen: Boolean = false,
    val confirmationHandled: Boolean = false,
    val failure: UpdateInstallFailure? = null,
) {
    fun validate() {
        require(UUID.fromString(id).version() == 4 && UUID.fromString(id).variant() == 2 && UUID.fromString(id).toString() == id && sessionId >= 0)
        failure?.validate()
    }

    fun encode() = buildJsonObject {
        put("attempt_id", id)
        put("session_id", sessionId)
        put("terminal_callback_seen", terminalCallbackSeen)
        put("confirmation_handled", confirmationHandled)
        put("failure", failure?.let { value ->
            buildJsonObject {
                put("code", value.code)
                put("stage", value.stage)
                put("at_ms", value.atMillis)
            }
        } ?: JsonNull)
    }

    companion object {
        fun decode(value: JsonObject): UpdateInstallAttempt {
            require(value.keys == setOf("attempt_id", "session_id", "terminal_callback_seen", "confirmation_handled", "failure"))
            val failure = value.getValue("failure").takeUnless { it == JsonNull }?.jsonObject?.let {
                require(it.keys == setOf("code", "stage", "at_ms"))
                UpdateInstallFailure(it.string("code")!!, it.string("stage")!!, it.long("at_ms")!!)
            }
            val session = value.long("session_id")!!
            require(session in 0..Int.MAX_VALUE.toLong())
            return UpdateInstallAttempt(
                value.string("attempt_id")!!, session.toInt(),
                requireNotNull((value["terminal_callback_seen"] as? JsonPrimitive)?.takeUnless { it.isString }?.booleanOrNull),
                requireNotNull((value["confirmation_handled"] as? JsonPrimitive)?.takeUnless { it.isString }?.booleanOrNull),
                failure,
            ).also { it.validate() }
        }
    }
}

/** The canonical maintenance record, including the last attempt after its active session ends. */
internal data class UpdateMaintenanceRecord(
    val updateId: String,
    val targetVersion: String,
    val targetVersionCode: Long,
    val targetApkSha256: String,
    val targetApkSize: Long,
    val targetApkSignerSha256: String,
    val phase: MaintenancePhase,
    val apkSessionId: Int?,
    val lastAttempt: UpdateInstallAttempt? = null,
) {
    fun validate(): UpdateMaintenanceRecord {
        require(UUID_V4.matches(updateId))
        require(SEMVER.matches(targetVersion) && targetVersionCode > 0)
        require(HEX64.matches(targetApkSignerSha256))
        require(HEX64.matches(targetApkSha256) && targetApkSize > 0)
        require(apkSessionId == null || (apkSessionId >= 0 && phase == MaintenancePhase.ApkInstalling))
        lastAttempt?.validate()
        require(lastAttempt == null || apkSessionId == null || lastAttempt.sessionId == apkSessionId)
        return this
    }

    fun encode(): String = buildJsonObject {
        put("schema_version", 2)
        put("update_id", updateId)
        put("target_version", targetVersion)
        put("target_version_code", targetVersionCode)
        put("target_apk_sha256", targetApkSha256)
        put("target_apk_size", targetApkSize)
        put("target_apk_signer_sha256", targetApkSignerSha256)
        put("phase", phase.wire)
        put("apk_session_id", apkSessionId)
        put("last_attempt", lastAttempt?.encode() ?: JsonNull)
    }.toString()

    companion object {
        private val UUID_V4 = Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")
        private val SEMVER = Regex("(0|[1-9]\\d{0,2})\\.(0|[1-9]\\d{0,2})\\.(0|[1-9]\\d{0,2})")
        private val HEX64 = Regex("[0-9a-f]{64}")
        private val KEYS = setOf(
            "schema_version", "update_id", "target_version", "target_version_code", "target_apk_sha256",
            "target_apk_size", "target_apk_signer_sha256", "phase", "apk_session_id",
        )

        fun newUpdateId(): String = UUID.randomUUID().toString()

        fun decode(text: String): UpdateMaintenanceRecord {
            val value = Json.parseToJsonElement(text) as JsonObject
            val schema = value.long("schema_version")
            require(
                (schema == 1L && value.keys == KEYS) ||
                    (schema == 2L && value.keys == KEYS + "last_attempt"),
            )
            return UpdateMaintenanceRecord(
                updateId = value.string("update_id")!!,
                targetVersion = value.string("target_version")!!,
                targetVersionCode = value.long("target_version_code")!!,
                targetApkSha256 = value.string("target_apk_sha256")!!,
                targetApkSize = value.long("target_apk_size")!!,
                targetApkSignerSha256 = value.string("target_apk_signer_sha256")!!,
                phase = MaintenancePhase.entries.single { it.wire == value.string("phase") },
                apkSessionId = value.long("apk_session_id")?.let { id ->
                    require(id in 0..Int.MAX_VALUE.toLong())
                    id.toInt()
                },
                lastAttempt = if (schema == 2L) value.getValue("last_attempt")
                    .takeUnless { it == JsonNull }?.jsonObject?.let(UpdateInstallAttempt::decode) else null,
            ).validate()
        }

    }
}

/**
 * HostController's single writer for the maintenance record. Every mutation takes
 * the stable `update-maintenance.lock`, re-reads the current record, writes a same-directory
 * owner-only temp, fsyncs it, atomically renames it and fsyncs the directory.
 */
internal class UpdateMaintenanceStore(
    private val base: File,
    private val restrictToOwner: (File) -> Unit,
    private val syncDirectory: (File) -> Unit,
) {
    private val record = File(base, RECORD)

    fun read(): UpdateMaintenanceRecord? = locked { current() }

    /** Creates the record only when none exists. */
    fun create(next: UpdateMaintenanceRecord) = locked {
        if (current() != null) throw MaintenanceStoreConflict()
        write(record, next.validate().encode())
    }

    /** Replaces exactly [expected]; a changed record refuses the mutation. */
    fun replace(expected: UpdateMaintenanceRecord, next: UpdateMaintenanceRecord) = locked {
        if (current() != expected) throw MaintenanceStoreConflict()
        check(next.updateId == expected.updateId)
        write(record, next.validate().encode())
    }

    fun delete(expected: UpdateMaintenanceRecord) = locked {
        if (current() != expected) throw MaintenanceStoreConflict()
        Files.delete(record.toPath())
        syncDirectory(base)
    }

    private fun current(): UpdateMaintenanceRecord? {
        if (!record.exists()) return null
        require(record.length() <= 16 * 1024) { "update maintenance record exceeds its bound" }
        return UpdateMaintenanceRecord.decode(record.readText())
    }

    private fun write(target: File, text: String) {
        val temp = File(base, ".${target.name}.${UUID.randomUUID()}.tmp")
        try {
            FileOutputStream(temp).use { output ->
                restrictToOwner(temp)
                output.write(text.encodeToByteArray())
                output.fd.sync()
            }
            Files.move(temp.toPath(), target.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
            syncDirectory(base)
        } finally {
            temp.delete()
        }
    }

    private fun <T> locked(action: () -> T): T {
        check(base.isDirectory || base.mkdirs()) { "canonical base is unavailable" }
        return FileChannel.open(File(base, LOCK).toPath(), StandardOpenOption.CREATE, StandardOpenOption.WRITE).use { channel ->
            channel.lock().use { action() }
        }
    }

    companion object {
        const val RECORD = "update-maintenance.json"
        private const val LOCK = "update-maintenance.lock"

        fun android(base: File) = UpdateMaintenanceStore(
            base = base,
            restrictToOwner = { file -> android.system.Os.chmod(file.absolutePath, "600".toInt(8)) },
            syncDirectory = { directory ->
                FileChannel.open(directory.toPath(), StandardOpenOption.READ).use { it.force(true) }
            },
        )
    }
}

private fun JsonObject.string(key: String): String? = when (val value = getValue(key)) {
    JsonNull -> null
    is JsonPrimitive -> value.also { require(it.isString) }.content
    else -> error("$key is not a string")
}

private fun JsonObject.long(key: String): Long? = when (val value = getValue(key)) {
    JsonNull -> null
    is JsonPrimitive -> requireNotNull(value.takeUnless { it.isString }?.longOrNull)
    else -> error("$key is not an integer")
}

internal class MaintenanceStoreConflict : IllegalStateException("update maintenance record changed")
