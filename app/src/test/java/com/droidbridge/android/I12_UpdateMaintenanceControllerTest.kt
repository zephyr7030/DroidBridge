package com.droidbridge.android

import com.droidbridge.android.product.release.ReleaseConfig
import com.droidbridge.android.runtimehost.ApkSessionInstaller
import com.droidbridge.android.runtimehost.ArchiveFacts
import com.droidbridge.android.runtimehost.InstalledPackageFacts
import com.droidbridge.android.runtimehost.MaintenanceHost
import com.droidbridge.android.runtimehost.MaintenancePhase
import com.droidbridge.android.runtimehost.ModuleObservation
import com.droidbridge.android.runtimehost.PrivilegedArtifact
import com.droidbridge.android.runtimehost.PrivilegedOutcome
import com.droidbridge.android.runtimehost.UpdateMaintenanceController
import com.droidbridge.android.runtimehost.UpdateMaintenanceRecord
import com.droidbridge.android.runtimehost.UpdateMaintenanceStore
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
import org.junit.Assert.assertNotNull
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
    private val apkBytes = ByteArray(4096) { (it % 251).toByte() }
    private val moduleBytes = ByteArray(2048) { (it % 13).toByte() }

    private inner class FakeHost : MaintenanceHost {
        var busy = false
        var admissionOpen = true
        var module = ModuleObservation.Absent
        var privileged = false
        var outcome = PrivilegedOutcome(0, cleanupVerified = true)
        var daemonCleanup: String? = null
        val dispatched = mutableListOf<Pair<PrivilegedArtifact, UpdateMaintenanceRecord>>()
        var onDispatch: () -> Unit = {}
        override fun ensureApkHost(): String? = null
        override fun closeAdmission(): String? = if (busy) "HOST_TRANSITION_PENDING" else null.also { admissionOpen = false }
        override fun reopenAdmission(): Boolean = true.also { admissionOpen = true }
        override fun moduleObservation() = module
        override fun cleanupVerified() = true
        override fun privilegedInstallAvailable() = privileged
        override fun privilegedInstall(kind: PrivilegedArtifact, record: UpdateMaintenanceRecord, artifact: File): PrivilegedOutcome {
            dispatched += kind to record
            assertEquals(record, store.read())
            onDispatch()
            return outcome
        }
        override fun privilegedCleanup(updateId: String) = daemonCleanup
    }

    private inner class FakePackages : InstalledPackageFacts {
        var versionCode = 999L
        override fun installedVersionCode() = versionCode
        override fun installedSignerSha256(): String = signer
        override fun archive(file: File) = ArchiveFacts("com.droidbridge.android", 1000, signer)
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
        override fun writeAndCommit(sessionId: Int, apk: File, size: Long, sha256: String) {
            recordedSessionAtWrite = store.read()?.apkSessionId
            check(!failWrite) { "write failed" }
        }
        override fun abandon(sessionId: Int) {
            live -= sessionId
        }
    }

    private val host = FakeHost()
    private val packages = FakePackages()
    private val installer = FakeInstaller()
    private val controller = UpdateMaintenanceController(
        packageName = "com.droidbridge.android",
        config = config,
        store = store,
        host = host,
        packages = packages,
        installer = installer,
        cacheRoot = cache,
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
                put("artifacts", JsonObject(mapOf("apk" to replaced("apk", apkBytes), "magisk" to replaced("magisk", moduleBytes))))
            },
        )
        File(cache, "0.1.0").mkdirs()
        File(cache, "0.1.0/droidbridge-0.1.0-arm64-v8a.apk").writeBytes(apkBytes)
        File(cache, "0.1.0/droidbridge-magisk-0.1.0.zip").writeBytes(moduleBytes)
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
        val unconfigured = UpdateMaintenanceController("com.droidbridge.android", ReleaseConfig.Unconfigured, store, host, packages, installer, cache)
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
        assertEquals("STALE_AUTHORITY", error(controller.installApk(id)))
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
    fun I12_G02_moduleStepCompletesOnlyFromObservationOrTheConfirmedApkOnlyExit() {
        val (manifest, signature) = signedRelease()
        host.module = ModuleObservation.Mismatched
        val id = updateId(controller.beginProductUpdate(manifest, signature))
        assertTrue(store.read()!!.requiresModule)
        controller.installApk(id)
        packages.versionCode = 1000
        controller.recover()
        assertEquals(MaintenancePhase.ModulePending, store.read()!!.phase)
        assertFalse(host.admissionOpen)

        assertNull(error(controller.continueWithoutModule(id)))
        assertNull(store.read())
        assertTrue(store.exclusionPresent())
        assertTrue(host.admissionOpen)
        assertEquals("excluded", Json.parseToJsonElement(controller.state()).jsonObject.getValue("module").jsonPrimitive.content)

        val repairId = updateId(controller.beginModuleRepair(manifest, signature))
        assertEquals(MaintenancePhase.ModulePending, store.read()!!.phase)
        controller.recover()
        assertTrue(store.exclusionPresent())
        host.module = ModuleObservation.Compatible
        controller.recover()
        assertNull(store.read())
        assertFalse(store.exclusionPresent())
        assertTrue(repairId.isNotEmpty())
    }

    @Test
    fun I12_G04_privilegedApkAttemptIsRecordedBeforeDispatchAndRetriesOnlyAfterCleanProof() {
        val (manifest, signature) = signedRelease()
        host.module = ModuleObservation.Compatible
        host.privileged = true
        val id = updateId(controller.beginProductUpdate(manifest, signature))
        assertEquals("magisk_privileged", Json.parseToJsonElement(controller.state()).jsonObject.getValue("record").jsonObject
            .getValue("apk_install_provider").jsonPrimitive.content)

        host.outcome = PrivilegedOutcome(1, cleanupVerified = true)
        assertEquals("IO_ERROR", error(controller.installApk(id)))
        val dispatched = host.dispatched.single().second
        assertEquals(MaintenancePhase.ApkInstalling, dispatched.phase)
        assertNotNull(dispatched.maintenanceExecutionId)
        assertEquals(MaintenancePhase.Prepared, store.read()!!.phase)
        assertNull(store.read()!!.maintenanceExecutionId)

        host.outcome = PrivilegedOutcome(null, cleanupVerified = false)
        assertEquals("IO_ERROR", error(controller.installApk(id)))
        val uncertain = store.read()!!
        assertEquals(MaintenancePhase.ApkInstalling, uncertain.phase)
        assertEquals("HOST_TRANSITION_PENDING", error(controller.cancel(id)))
        assertEquals("INVALID_ARGUMENT", error(controller.installApk(id)))

        host.daemonCleanup = "unverified"
        controller.recover()
        assertEquals(uncertain, store.read())
        host.daemonCleanup = "clean"
        controller.recover()
        assertEquals(MaintenancePhase.Prepared, store.read()!!.phase)

        host.onDispatch = {
            packages.versionCode = 1000
            // The replaced APK no longer matches the old module until the module is updated too.
            host.module = ModuleObservation.Mismatched
        }
        host.outcome = PrivilegedOutcome(0, cleanupVerified = true)
        controller.installApk(id)
        assertEquals(MaintenancePhase.ModulePending, store.read()!!.phase)
        assertNull(store.read()!!.maintenanceExecutionId)
    }

    @Test
    fun I12_G04_privilegedModuleInstallWaitsForTheReloadedModuleObservation() {
        val (manifest, signature) = signedRelease()
        packages.versionCode = 1000
        host.module = ModuleObservation.Mismatched
        host.privileged = true
        val id = updateId(controller.beginModuleRepair(manifest, signature))

        host.outcome = PrivilegedOutcome(0, cleanupVerified = true)
        assertNull(error(controller.installModule(id)))
        assertEquals(PrivilegedArtifact.Module, host.dispatched.single().first)
        val settled = store.read()!!
        assertEquals(MaintenancePhase.ModuleInstalling, settled.phase)
        assertNull(settled.maintenanceExecutionId)
        assertFalse(host.admissionOpen)

        controller.recover()
        assertEquals(settled, store.read())
        host.module = ModuleObservation.Compatible
        controller.recover()
        assertNull(store.read())
        assertTrue(host.admissionOpen)
    }

    private fun sha256(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
}
