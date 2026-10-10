package com.droidbridge.standalone

import com.droidbridge.standalone.product.release.ReleaseConfig
import com.droidbridge.standalone.runtimehost.ApkSessionInstaller
import com.droidbridge.standalone.runtimehost.ArchiveFacts
import com.droidbridge.standalone.runtimehost.InstalledPackageFacts
import com.droidbridge.standalone.runtimehost.MaintenanceHost
import com.droidbridge.standalone.runtimehost.MaintenancePhase
import com.droidbridge.standalone.runtimehost.UpdateMaintenanceController
import com.droidbridge.standalone.runtimehost.UpdateMaintenanceRecord
import com.droidbridge.standalone.runtimehost.UpdateMaintenanceStore
import com.droidbridge.standalone.runtimehost.UpdateInstallAttempt
import com.droidbridge.standalone.runtimehost.UpdateInstallerCallback
import com.droidbridge.standalone.product.update.UpdateMaintenanceReplies
import com.droidbridge.standalone.product.update.MaintenanceReply
import java.io.IOException
import org.junit.Assert.assertThrows
import java.io.File
import java.nio.file.Files
import java.security.KeyFactory
import java.security.MessageDigest
import java.security.Signature
import java.security.spec.PKCS8EncodedKeySpec
import java.util.Base64
import java.util.Properties
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class I12_UpdateMaintenanceControllerTest {
    private val properties = Properties().apply { File("../release-config.properties").inputStream().use(::load) }
    private val signer = properties.getProperty("apk_signer_sha256")
    private val config = ReleaseConfig.from(
        properties.getProperty("github_owner"),
        properties.getProperty("github_repo"),
        properties.getProperty("manifest_url"),
        signer,
        properties.getProperty("release_key_id"),
        Base64.getEncoder().encodeToString(File("../tools/fixtures/release/manifest-test-public-key.der").readBytes()),
    )
    private val base = Files.createTempDirectory("canonical").toFile()
    private val cache = Files.createTempDirectory("updates").toFile()
    private val store = UpdateMaintenanceStore(base, restrictToOwner = {}, syncDirectory = {})
    private var nowMillis = System.currentTimeMillis()
    private val apkBytes = ByteArray(4096) { (it % 251).toByte() }

    private inner class FakeHost : MaintenanceHost {
        var busy = false
        var closeFailure: String? = null
        var admissionOpen = true
        override fun ensureApkHost(): String? = null
        override fun closeAdmission(): String? = if (closeFailure != null) closeFailure else if (busy) "HOST_TRANSITION_PENDING" else null.also { admissionOpen = false }
        override fun reopenAdmission(): Boolean = true.also { admissionOpen = true }
    }

    private inner class FakePackages : InstalledPackageFacts {
        var versionCode = 999L
        var actualSigner = signer
        override fun installedVersionCode() = versionCode
        override fun installedSignerSha256(): String = actualSigner
        override fun archive(file: File) = ArchiveFacts("com.droidbridge.standalone", 1000, signer)
    }

    private inner class FakeInstaller : ApkSessionInstaller {
        val live = mutableSetOf<Int>()
        var nextId = 7
        var failWrite = false
        var recordedSessionAtWrite: Int? = null
        override fun abandonUnrecordedSessions(recorded: Int?) {
            live.retainAll(setOfNotNull(recorded))
        }
        override fun create(size: Long) = nextId++.also { live += it }
        override fun exists(sessionId: Int) = sessionId in live
        override fun writeAndCommit(updateId: String, attempt: UpdateInstallAttempt, apk: File, size: Long, sha256: String) {
            recordedSessionAtWrite = store.read()?.apkSessionId
            if (failWrite) throw IOException("write failed")
        }
        override fun abandon(sessionId: Int) {
            live -= sessionId
        }
    }

    private val host = FakeHost()
    private val packages = FakePackages()
    private val installer = FakeInstaller()
    private val controller = UpdateMaintenanceController(
        packageName = "com.droidbridge.standalone",
        config = config,
        store = store,
        host = host,
        packages = packages,
        installer = installer,
        cacheRoot = cache,
        nowMillis = { nowMillis },
    )

    /** The fixture manifest re-signed with the test key over synthetic artifact bytes cached locally. */
    private fun signedRelease(): Pair<ByteArray, ByteArray> {
        val root = Json.parseToJsonElement(File("../tools/fixtures/release/sample-0.1.0/release.json").readText()).jsonObject
        val artifacts = root.getValue("artifacts").jsonObject
        fun replaced(key: String, bytes: ByteArray) = JsonObject(
            artifacts.getValue(key).jsonObject.toMutableMap().apply {
                put("size", JsonPrimitive(bytes.size))
                put("sha256", JsonPrimitive(sha256(bytes)))
            },
        )
        val manifest = JsonObject(
            root.toMutableMap().apply {
                put("artifacts", JsonObject(mapOf("apk" to replaced("apk", apkBytes))))
            },
        )
        File(cache, "0.1.0").mkdirs()
        File(cache, "0.1.0/droidbridge-0.1.0-arm64-v8a.apk").writeBytes(apkBytes)
        val bytes = (manifest.toString() + "\n").encodeToByteArray()
        val pem = File("../tools/fixtures/release/manifest-test-private-key.pem").readText()
            .replace("-----BEGIN PRIVATE KEY-----", "").replace("-----END PRIVATE KEY-----", "").replace(Regex("\\s"), "")
        val key = KeyFactory.getInstance("EC").generatePrivate(PKCS8EncodedKeySpec(Base64.getDecoder().decode(pem)))
        val signature = Signature.getInstance("SHA256withECDSA").run {
            initSign(key)
            update(bytes)
            sign()
        }
        return bytes to signature
    }

    private fun updateId(reply: String) = Json.parseToJsonElement(reply).jsonObject.getValue("record").jsonObject
        .getValue("update_id").jsonPrimitive.content

    private fun error(reply: String) = Json.parseToJsonElement(reply).jsonObject["error"]?.jsonPrimitive?.content

    @Test
    fun I12_G02_busyRuntimeNeverCreatesAMaintenanceRecord() {
        val (manifest, signature) = signedRelease()
        host.busy = true
        assertEquals("HOST_TRANSITION_PENDING", error(controller.beginProductUpdate(manifest, signature)))
        assertNull(store.read())
        assertTrue(host.admissionOpen)
    }

    @Test
    fun I12_G02_unverifiedArtifactsOrUnconfiguredBuildsAreRefused() {
        val (manifest, signature) = signedRelease()
        File(cache, "0.1.0/droidbridge-0.1.0-arm64-v8a.apk").writeBytes(ByteArray(4096))
        assertEquals("INVALID_ARGUMENT", error(controller.beginProductUpdate(manifest, signature)))
        val unconfigured = UpdateMaintenanceController("com.droidbridge.standalone", ReleaseConfig.Unconfigured, store, host, packages, installer, cache)
        assertEquals("CAPABILITY_UNAVAILABLE", error(unconfigured.beginProductUpdate(manifest, signature)))
        assertNull(store.read())
    }

    @Test
    fun I12_G02_packageInstallerSessionIsDurableBeforeBytesAndReplacementCompletes() {
        val (manifest, signature) = signedRelease()
        val id = updateId(controller.beginProductUpdate(manifest, signature))
        assertFalse(host.admissionOpen)
        assertEquals(MaintenancePhase.Prepared, store.read()!!.phase)

        assertNull(error(controller.installApk(id)))
        assertEquals(7, installer.recordedSessionAtWrite)
        assertEquals(MaintenancePhase.ApkInstalling, store.read()!!.phase)
        assertEquals("INVALID_ARGUMENT", error(controller.installApk(id)))

        packages.versionCode = 1000
        controller.recover()
        assertNull(store.read())
        assertTrue(host.admissionOpen)
    }

    @Test
    fun I12_G02_lostOrFailedSessionsReturnToPreparedForExplicitRetry() {
        val (manifest, signature) = signedRelease()
        val id = updateId(controller.beginProductUpdate(manifest, signature))
        installer.failWrite = true
        assertEquals("IO_ERROR", error(controller.installApk(id)))
        assertEquals(MaintenancePhase.Prepared, store.read()!!.phase)
        assertTrue(installer.live.isEmpty())

        installer.failWrite = false
        controller.installApk(id)
        installer.live.clear()
        controller.recover()
        val record = store.read()!!
        assertEquals(MaintenancePhase.Prepared, record.phase)
        assertNull(record.apkSessionId)
        assertFalse(host.admissionOpen)

        assertNull(error(controller.cancel(id)))
        assertNull(store.read())
        assertTrue(host.admissionOpen)
    }

    @Test
    fun I12_G02_aWrongPackageIsNeverRecorded() {
        val (manifest, signature) = signedRelease()
        val other = UpdateMaintenanceController("com.droidbridge.root", config, store, host, packages, installer, cache)
        assertEquals("INVALID_ARGUMENT", error(other.beginProductUpdate(manifest, signature)))
        assertNull(store.read())
        assertTrue(host.admissionOpen)
    }

    private fun installing(): UpdateInstallerCallback {
        val (manifest, signature) = signedRelease()
        val id = updateId(controller.beginProductUpdate(manifest, signature))
        assertNull(error(controller.installApk(id)))
        val attempt = store.read()!!.lastAttempt!!
        return UpdateInstallerCallback(id, attempt.id, attempt.sessionId, 6)
    }

    @Test
    fun preparedExternalReplacementReconcilesOnlyMatchingVersionAndSigner() {
        val (manifest, signature) = signedRelease()
        controller.beginProductUpdate(manifest, signature)
        packages.versionCode = 1000
        packages.actualSigner = "0".repeat(64)
        controller.recover()
        assertEquals(MaintenancePhase.Prepared, store.read()!!.phase)
        assertFalse(host.admissionOpen)
        packages.actualSigner = signer
        controller.recover()
        assertNull(store.read())
        assertTrue(host.admissionOpen)
    }

    @Test
    fun admissionRefusalPreservesStageAndNeverCreatesInstallerWork() {
        val (manifest, signature) = signedRelease()
        host.closeFailure = "IO_ERROR"
        val refusal = UpdateMaintenanceReplies.mutation(controller.beginProductUpdate(manifest, signature))
        assertEquals(MaintenanceReply.Refused("IO_ERROR", "close_admission"), refusal)
        assertNull(store.read())
        assertTrue(installer.live.isEmpty())
    }

    @Test
    fun installerFailureSurvivesEitherCallbackRecoveryOrder() {
        for (recoverFirst in listOf(false, true)) {
            val callback = installing()
            installer.live.clear()
            if (recoverFirst) controller.recover()
            assertTrue(controller.installerResult(callback, { true }, null))
            controller.recover()
            val record = store.read()!!
            assertEquals(MaintenancePhase.Prepared, record.phase)
            assertNull(record.apkSessionId)
            assertEquals(callback.attemptId, record.lastAttempt!!.id)
            assertEquals("INSTALLER_STORAGE", record.lastAttempt.failure!!.code)
            assertEquals("INSTALLER_STORAGE", UpdateMaintenanceReplies.state(controller.state())!!.record!!.installFailure!!.code)
            assertFalse(host.admissionOpen)
            assertNull(error(controller.cancel(callback.updateId)))
        }
    }

    @Test
    fun retryRejectsOldCallbackEvenWhenThePlatformReusesSessionId() {
        val old = installing()
        installer.live.clear()
        controller.recover()
        installer.nextId = old.sessionId
        assertNull(error(controller.installApk(old.updateId)))
        val newAttempt = store.read()!!.lastAttempt!!
        assertFalse(old.attemptId == newAttempt.id)
        assertEquals(old.sessionId, newAttempt.sessionId)
        var launches = 0
        assertFalse(controller.installerResult(old.copy(status = -1), { true }) { launches++ })
        assertFalse(controller.installerResult(old, { true }, null))
        assertEquals(0, launches)
        assertNull(store.read()!!.lastAttempt!!.failure)
        controller.cancel(old.updateId)
        assertFalse(controller.installerResult(old.copy(attemptId = newAttempt.id), { true }) { launches++ })
        assertNull(store.read())
    }

    @Test
    fun duplicateOrTerminalCallbacksCannotReopenConfirmationOrExtendRetention() {
        val callback = installing()
        var launches = 0
        val pending = callback.copy(status = -1)
        assertTrue(controller.installerResult(pending, { true }) { launches++ })
        assertFalse(controller.installerResult(pending, { true }) { launches++ })
        assertEquals(1, launches)
        assertTrue(controller.installerResult(callback, { true }, null))
        val failure = store.read()!!.lastAttempt!!.failure
        nowMillis += 1000
        assertFalse(controller.installerResult(callback.copy(status = 2), { true }, null))
        assertFalse(controller.installerResult(pending, { true }) { launches++ })
        assertEquals(failure, store.read()!!.lastAttempt!!.failure)
        assertEquals(1, launches)
    }

    @Test
    fun expiredOrUnavailableConfirmationNeverLaunchesAndDoesNotMarkSuccess() {
        val callback = installing()
        var launches = 0
        assertFalse(controller.installerResult(callback.copy(status = -1), { false }) { launches++ })
        assertFalse(store.read()!!.lastAttempt!!.confirmationHandled)
        assertTrue(controller.installerResult(callback.copy(status = -1), { true }, null))
        assertEquals("CONFIRMATION_MISSING", store.read()!!.lastAttempt!!.failure!!.code)
        assertEquals(0, launches)
        assertFalse(store.read()!!.lastAttempt!!.terminalCallbackSeen)
    }

    @Test
    fun missingUnknownAndSuccessStatusesNeverProveAnInstallation() {
        val callback = installing()
        assertTrue(controller.installerResult(callback.copy(status = null), { true }, null))
        assertEquals("INSTALLER_STATUS_MISSING", store.read()!!.lastAttempt!!.failure!!.code)
        assertTrue(controller.installerResult(callback.copy(status = 999), { true }, null))
        assertEquals("INSTALLER_STATUS_UNKNOWN", store.read()!!.lastAttempt!!.failure!!.code)
        assertFalse(store.read()!!.lastAttempt!!.terminalCallbackSeen)
        assertTrue(controller.installerResult(callback.copy(status = 0), { true }, null))
        controller.recover()
        assertEquals(MaintenancePhase.ApkInstalling, store.read()!!.phase)
        assertFalse(host.admissionOpen)
        packages.versionCode = 1000
        controller.recover()
        assertNull(store.read())
        assertTrue(host.admissionOpen)
    }

    @Test
    fun expiredOrFutureReceiptIsRemovedWithoutDroppingAttemptIdentity() {
        val callback = installing()
        assertTrue(controller.installerResult(callback.copy(status = 8), { true }, null))
        assertEquals("INSTALLER_TIMEOUT", store.read()!!.lastAttempt!!.failure!!.code)
        nowMillis -= 1
        controller.state()
        assertNull(store.read()!!.lastAttempt!!.failure)
        assertEquals(callback.attemptId, store.read()!!.lastAttempt!!.id)
        assertTrue(store.read()!!.lastAttempt!!.terminalCallbackSeen)
        controller.cancel(callback.updateId)
        val next = installing()
        controller.installerResult(next, { true }, null)
        nowMillis += 24L * 60 * 60 * 1000 + 1
        controller.state()
        assertNull(store.read()!!.lastAttempt!!.failure)
        assertEquals(next.attemptId, store.read()!!.lastAttempt!!.id)
    }

    @Test
    fun cleanupProtectsPreparedArtifactsAndRefusesUnknownMaintenance() {
        val (manifest, signature) = signedRelease()
        val id = updateId(controller.beginProductUpdate(manifest, signature))
        val apk = File(cache, "0.1.0/droidbridge-0.1.0-arm64-v8a.apk")
        assertTrue(apk.setLastModified(nowMillis - 25L * 60 * 60 * 1000))
        val other = File(cache, "old.apk").apply { writeText("old"); setLastModified(nowMillis - 25L * 60 * 60 * 1000) }
        controller.cleanupDownloads()
        assertTrue(apk.exists())
        assertFalse(other.exists())
        assertNull(error(controller.installApk(id)))
        File(base, UpdateMaintenanceStore.RECORD).writeText("invalid")
        assertThrows(Exception::class.java) { controller.cleanupDownloads() }
        assertTrue(apk.exists())
    }

    @Test
    fun synchronousFailureReceiptSurvivesAbandonCallbackUntilExplicitRetry() {
        val (manifest, signature) = signedRelease()
        val id = updateId(controller.beginProductUpdate(manifest, signature))
        installer.failWrite = true
        val reply = UpdateMaintenanceReplies.mutation(controller.installApk(id))
        assertEquals(MaintenanceReply.Refused("IO_ERROR", "apk_write_commit"), reply)
        val failed = store.read()!!.lastAttempt!!
        assertTrue(failed.terminalCallbackSeen)
        assertEquals("IO_ERROR", failed.failure!!.code)
        assertFalse(controller.installerResult(UpdateInstallerCallback(id, failed.id, failed.sessionId, 3), { true }, null))
        installer.failWrite = false
        assertNull(error(controller.installApk(id)))
        assertNull(store.read()!!.lastAttempt!!.failure)
        assertFalse(store.read()!!.lastAttempt!!.id == failed.id)
    }

    @Test
    fun failedReceiptPersistenceStillAbandonsTheSessionAndCanRecover() {
        var writes = 0
        val fragile = UpdateMaintenanceStore(base, restrictToOwner = {
            writes++
            if (writes == 3) throw IOException("receipt unavailable")
        }, syncDirectory = {})
        val other = UpdateMaintenanceController(
            "com.droidbridge.standalone", config, fragile, host, packages, installer, cache,
        )
        val (manifest, signature) = signedRelease()
        val id = updateId(other.beginProductUpdate(manifest, signature))
        installer.failWrite = true
        assertEquals(
            MaintenanceReply.Refused("IO_ERROR", "record_install_failure"),
            UpdateMaintenanceReplies.mutation(other.installApk(id)),
        )
        assertTrue(installer.live.isEmpty())
        assertFalse(host.admissionOpen)
        other.recover()
        assertEquals(MaintenancePhase.Prepared, fragile.read()!!.phase)
        assertNull(fragile.read()!!.apkSessionId)
    }

    private fun sha256(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
}
