package com.droidbridge.standalone

import com.droidbridge.standalone.runtimehost.MaintenancePhase
import com.droidbridge.standalone.runtimehost.UpdateMaintenanceRecord
import com.droidbridge.standalone.runtimehost.UpdateMaintenanceStore
import com.droidbridge.standalone.runtimehost.UpdateInstallAttempt
import com.droidbridge.standalone.runtimehost.UpdateInstallFailure
import kotlinx.serialization.json.*
import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class I12_UpdateMaintenanceStoreTest {
    private val base = Files.createTempDirectory("canonical").toFile()
    private val store = UpdateMaintenanceStore(base, restrictToOwner = {}, syncDirectory = {})
    private val sha = "a".repeat(64)

    private val prepared = UpdateMaintenanceRecord(
        updateId = "3f0c2b8e-1d2a-4c3b-9a8e-0f1e2d3c4b5a",
        targetVersion = "0.2.0",
        targetVersionCode = 2000,
        targetApkSha256 = sha,
        targetApkSize = 10,
        targetApkSignerSha256 = sha,
        phase = MaintenancePhase.Prepared,
        apkSessionId = null,
    )

    @Test
    fun I12_G02_recordRoundTripsExactlyAndRejectsInconsistentPhases() {
        store.create(prepared)
        assertEquals(prepared, store.read())
        assertEquals(prepared, UpdateMaintenanceRecord.decode(File(base, UpdateMaintenanceStore.RECORD).readText()))
        assertThrows(IllegalStateException::class.java) { store.create(prepared) }

        val installing = prepared.copy(phase = MaintenancePhase.ApkInstalling, apkSessionId = 42)
        store.replace(prepared, installing)
        assertThrows(IllegalStateException::class.java) { store.replace(prepared, installing) }
        assertThrows(IllegalArgumentException::class.java) {
            store.replace(installing, installing.copy(phase = MaintenancePhase.Prepared))
        }
        store.delete(installing)
        assertNull(store.read())
        assertTrue(base.listFiles()!!.none { it.name.endsWith(".tmp") })
    }

    @Test
    fun legacyRecordsRemainReadableWithoutInventingCallbackIdentity() {
        val legacy = JsonObject(Json.parseToJsonElement(prepared.encode()).jsonObject.toMutableMap().apply {
            put("schema_version", JsonPrimitive(1))
            remove("last_attempt")
        })
        assertEquals(prepared, UpdateMaintenanceRecord.decode(legacy.toString()))
        val installing = JsonObject(legacy.toMutableMap().apply {
            put("phase", JsonPrimitive("apk_installing"))
            put("apk_session_id", JsonPrimitive(42))
        })
        val record = UpdateMaintenanceRecord.decode(installing.toString())
        assertNull(record.lastAttempt)
        assertEquals(42, record.apkSessionId)
    }

    @Test
    fun retainedAttemptRoundTripsAndRejectsMismatchedActiveSession() {
        val attempt = UpdateInstallAttempt(
            "0a7e3a6e-bbcc-4f56-89ab-3fcd7a863321", 7,
            terminalCallbackSeen = true,
            failure = UpdateInstallFailure("INSTALLER_STORAGE", "installer_result", 1234),
        )
        val failed = prepared.copy(lastAttempt = attempt)
        assertEquals(failed, UpdateMaintenanceRecord.decode(failed.encode()))
        assertThrows(IllegalArgumentException::class.java) {
            failed.copy(phase = MaintenancePhase.ApkInstalling, apkSessionId = 8).validate()
        }
        assertThrows(IllegalArgumentException::class.java) {
            failed.copy(lastAttempt = attempt.copy(failure = UpdateInstallFailure("private/path", "installer_result", 0))).validate()
        }
    }

    @Test
    fun I12_G02_malformedRecordsAreNeverGuessed() {
        val file = File(base, UpdateMaintenanceStore.RECORD)
        file.writeText(prepared.encode().replace("\"phase\":\"prepared\"", "\"phase\":\"finished\""))
        assertThrows(NoSuchElementException::class.java) { store.read() }
        file.writeText(prepared.encode().replace("}", ",\"extra\":1}"))
        assertThrows(IllegalArgumentException::class.java) { store.read() }
    }
}
