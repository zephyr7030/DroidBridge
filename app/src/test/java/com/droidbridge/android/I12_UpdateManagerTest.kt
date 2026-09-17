package com.droidbridge.android

import com.droidbridge.android.product.update.ModulePresence
import com.droidbridge.android.product.update.ReleaseClassification
import com.droidbridge.android.product.update.ReleaseConfig
import com.droidbridge.android.product.update.ReleaseTransport
import com.droidbridge.android.product.update.UpdateCheck
import com.droidbridge.android.product.update.UpdateManager
import java.io.ByteArrayInputStream
import java.io.File
import java.io.InputStream
import java.nio.file.Files
import java.security.MessageDigest
import java.util.Base64
import java.util.Properties
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class I12_UpdateManagerTest {
    private val properties = Properties().apply { File("../release-config.properties").inputStream().use(::load) }
    private val fixture = File("../tools/fixtures/release/sample-0.1.0")
    private val config = ReleaseConfig.from(
        properties.getProperty("github_owner"),
        properties.getProperty("github_repo"),
        properties.getProperty("manifest_url"),
        properties.getProperty("apk_signer_sha256"),
        properties.getProperty("release_key_id"),
        Base64.getEncoder().encodeToString(File("../tools/fixtures/release/manifest-test-public-key.der").readBytes()),
    )

    /** Serves the signed fixture manifest and fixed artifact bytes; records every requested URL. */
    private class FakeTransport(private val artifacts: Map<String, ByteArray>, manifestDir: File) : ReleaseTransport {
        val requests = mutableListOf<String>()
        private val manifest = File(manifestDir, "release.json").readBytes()
        private val signature = File(manifestDir, "release.json.sig").readBytes()

        override fun <T> get(url: String, maxBytes: Long, read: (InputStream) -> T): T {
            requests += url
            val bytes = when {
                url.endsWith("release.json") -> manifest
                url.endsWith("release.json.sig") -> signature
                else -> artifacts[url.substringAfterLast('/')] ?: error("unexpected $url")
            }
            return read(ByteArrayInputStream(bytes))
        }
    }

    @Test
    fun I12_G01_unconfiguredBuildsNeverTouchTheNetwork() = runBlocking {
        val transport = FakeTransport(emptyMap(), fixture)
        val cache = Files.createTempDirectory("updates").toFile()
        val manager = UpdateManager(ReleaseConfig.Unconfigured, 999, cache, transport)
        manager.check(ModulePresence.Absent)
        assertEquals(UpdateCheck.Unconfigured, manager.state.value.check)
        assertTrue(transport.requests.isEmpty())
    }

    @Test
    fun I12_G01_checkClassifiesAndHashMismatchedDownloadsNeverBecomeVerified() = runBlocking {
        val cache = Files.createTempDirectory("updates").toFile()
        val wrongApk = ByteArray(32) { 7 }
        val manager = UpdateManager(config, 999, cache, FakeTransport(mapOf("droidbridge-0.1.0-arm64-v8a.apk" to wrongApk), fixture))
        manager.check(ModulePresence.Absent)
        val checked = manager.state.value.check as UpdateCheck.Checked
        assertTrue(checked.classification is ReleaseClassification.ProductUpdate)
        assertTrue(manager.state.value.newerVersionAvailable)

        manager.download(moduleRequired = false)
        assertTrue(manager.state.value.downloadFailed)
        assertNull(manager.state.value.downloads)
        assertFalse(File(cache, "0.1.0/droidbridge-0.1.0-arm64-v8a.apk").exists())
        assertFalse(File(cache, "0.1.0/.droidbridge-0.1.0-arm64-v8a.apk.part").exists())
    }

    @Test
    fun I12_G01_moduleRepairDownloadsOnlyTheVerifiedModule() = runBlocking {
        val cache = Files.createTempDirectory("updates").toFile()
        val manifest = File(fixture, "release.json").readText()
        val moduleSize = Regex("\"magisk\":\\{[^}]*\"size\":(\\d+)").find(manifest)!!.groupValues[1].toInt()
        val moduleSha = Regex("\"magisk\":\\{[^}]*\"sha256\":\"([0-9a-f]{64})\"").find(manifest)!!.groupValues[1]
        // The fixture digest names the real module ZIP; a synthetic body cannot satisfy it, so this
        // proves the repair path requests only the module artifact and rejects anything else.
        val transport = FakeTransport(mapOf("droidbridge-magisk-0.1.0.zip" to ByteArray(moduleSize)), fixture)
        val manager = UpdateManager(config, 1000, cache, transport)
        manager.check(ModulePresence.Mismatched)
        assertTrue((manager.state.value.check as UpdateCheck.Checked).classification is ReleaseClassification.ModuleRepair)
        assertFalse(manager.state.value.newerVersionAvailable)
        manager.download(moduleRequired = true)
        assertEquals(listOf("release.json", "release.json.sig", "droidbridge-magisk-0.1.0.zip"), transport.requests.map { it.substringAfterLast('/') })
        assertTrue(manager.state.value.downloadFailed)
        assertTrue(sha256(ByteArray(moduleSize)) != moduleSha)
    }

    @Test
    fun I12_G01_cleanupRemovesStaleAndPartialFilesButKeepsReferencedArtifacts() {
        val cache = Files.createTempDirectory("updates").toFile()
        val now = 10L * 24 * 60 * 60 * 1000
        val old = File(cache, "0.1.0/old.apk").apply { parentFile?.mkdirs(); writeText("old"); setLastModified(now - 25L * 60 * 60 * 1000) }
        val part = File(cache, "0.1.0/.x.part").apply { writeText("part"); setLastModified(now - 25L * 60 * 60 * 1000) }
        val referenced = File(cache, "0.1.0/keep.zip").apply { writeText("keep"); setLastModified(now - 48L * 60 * 60 * 1000) }
        val fresh = File(cache, "0.1.0/fresh.apk").apply { writeText("fresh"); setLastModified(now - 60_000) }
        UpdateManager(config, 999, cache, FakeTransport(emptyMap(), fixture), clock = { now }).cleanup(setOf(referenced))
        assertFalse(old.exists())
        assertFalse(part.exists())
        assertTrue(referenced.exists())
        assertTrue(fresh.exists())
    }

    private fun sha256(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
}
