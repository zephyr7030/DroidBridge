package com.droidbridge.android

import com.droidbridge.android.product.update.ModulePresence
import com.droidbridge.android.product.update.ReleaseClassification
import com.droidbridge.android.product.update.ReleaseConfig
import com.droidbridge.android.product.update.ReleaseManifests
import com.droidbridge.android.product.update.ReleaseRejected
import java.io.File
import java.security.KeyFactory
import java.security.Signature
import java.security.spec.PKCS8EncodedKeySpec
import java.util.Base64
import java.util.Properties
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class I12_ReleaseManifestTest {
    private val properties = Properties().apply { File("../release-config.properties").inputStream().use(::load) }
    private val fixture = File("../tools/fixtures/release/sample-0.1.0")
    private val manifestBytes = File(fixture, "release.json").readBytes()
    private val signature = File(fixture, "release.json.sig").readBytes()
    private val testPublicKey = File("../tools/fixtures/release/manifest-test-public-key.der").readBytes()

    private fun config(publicKeyBase64: String = Base64.getEncoder().encodeToString(testPublicKey)) = ReleaseConfig.from(
        properties.getProperty("github_owner"),
        properties.getProperty("github_repo"),
        properties.getProperty("manifest_url"),
        properties.getProperty("apk_signer_sha256"),
        properties.getProperty("release_key_id"),
        publicKeyBase64,
    ) as ReleaseConfig.Configured

    private fun signWithTestKey(bytes: ByteArray): ByteArray {
        val pem = File("../tools/fixtures/release/manifest-test-private-key.pem").readText()
            .replace("-----BEGIN PRIVATE KEY-----", "").replace("-----END PRIVATE KEY-----", "").replace(Regex("\\s"), "")
        val key = KeyFactory.getInstance("EC").generatePrivate(PKCS8EncodedKeySpec(Base64.getDecoder().decode(pem)))
        return Signature.getInstance("SHA256withECDSA").run {
            initSign(key)
            update(bytes)
            sign()
        }
    }

    private fun mutated(transform: (MutableMap<String, kotlinx.serialization.json.JsonElement>) -> Unit): ByteArray {
        val root = Json.parseToJsonElement(manifestBytes.decodeToString()) as JsonObject
        val changed = root.toMutableMap().also(transform)
        return (JsonObject(changed).toString() + "\n").encodeToByteArray()
    }

    @Test
    fun I12_G01_signedManifestVerifiesAndExposesExactArtifacts() {
        val manifest = ReleaseManifests.verify(config(), manifestBytes, signature)
        assertEquals("0.1.0", manifest.version)
        assertEquals(1000L, manifest.versionCode)
        assertEquals("droidbridge-0.1.0-arm64-v8a.apk", manifest.apk.name)
        assertEquals("droidbridge-magisk-0.1.0.zip", manifest.module.name)
        assertTrue(ReleaseManifests.HEX64.matches(manifest.module.sha256))
    }

    @Test
    fun I12_G01_tamperedBytesOrTheWrongKeyAreRejected() {
        val tampered = manifestBytes.copyOf().also { it[it.size - 3] = 'x'.code.toByte() }
        assertThrows(ReleaseRejected::class.java) { ReleaseManifests.verify(config(), tampered, signature) }
        val stable = config(properties.getProperty("release_public_key_base64"))
        assertThrows(ReleaseRejected::class.java) { ReleaseManifests.verify(stable, manifestBytes, signature) }
        assertThrows(ReleaseRejected::class.java) { ReleaseManifests.verify(config(), manifestBytes, byteArrayOf(0x30, 0x02, 0x00)) }
    }

    @Test
    fun I12_G01_validlySignedButInconsistentFactsAreRejected() {
        val cases = listOf(
            mutated { it["expiry"] = JsonPrimitive("2030-01-01T00:00:00Z") },
            mutated { it["channel"] = JsonPrimitive("beta") },
            mutated { it["version_code"] = JsonPrimitive(1001) },
            mutated { root ->
                val provenance = (root.getValue("provenance") as JsonObject).toMutableMap()
                provenance["apk_signer_sha256"] = JsonPrimitive("0".repeat(64))
                root["provenance"] = JsonObject(provenance)
            },
            mutated { root ->
                val artifacts = (root.getValue("artifacts") as JsonObject).toMutableMap()
                val apk = (artifacts.getValue("apk") as JsonObject).toMutableMap()
                apk["url"] = JsonPrimitive("https://example.com/droidbridge-0.1.0-arm64-v8a.apk")
                artifacts["apk"] = JsonObject(apk)
                root["artifacts"] = JsonObject(artifacts)
            },
        )
        cases.forEach { bytes ->
            assertThrows(ReleaseRejected::class.java) { ReleaseManifests.verify(config(), bytes, signWithTestKey(bytes)) }
        }
    }

    @Test
    fun I12_G01_classificationNeverOffersALowerOrSameVersionApk() {
        val manifest = ReleaseManifests.verify(config(), manifestBytes, signature)
        assertThrows(ReleaseRejected::class.java) { ReleaseManifests.classify(manifest, 1001, ModulePresence.Compatible) }
        assertSame(ReleaseClassification.UpToDate, ReleaseManifests.classify(manifest, 1000, ModulePresence.Compatible))
        listOf(ModulePresence.Absent, ModulePresence.Mismatched, ModulePresence.Excluded).forEach { module ->
            assertEquals(ReleaseClassification.ModuleRepair(manifest), ReleaseManifests.classify(manifest, 1000, module))
        }
        assertEquals(ReleaseClassification.ProductUpdate(manifest), ReleaseManifests.classify(manifest, 999, ModulePresence.Absent))
    }

    @Test
    fun I12_G01_releaseConfigurationAndHostsAreClosed() {
        assertSame(
            ReleaseConfig.Unconfigured,
            ReleaseConfig.from("UNCONFIGURED", "UNCONFIGURED", "UNCONFIGURED", "UNCONFIGURED", "UNCONFIGURED", "UNCONFIGURED"),
        )
        assertSame(
            ReleaseConfig.Unconfigured,
            ReleaseConfig.from("zephyr7030", "DroidBridge", "https://example.com/release.json", "0".repeat(64), "p256-x", properties.getProperty("release_public_key_base64")),
        )
        assertTrue(ReleaseManifests.allowedUrl("https://release-assets.githubusercontent.com/a/b"))
        assertFalse(ReleaseManifests.allowedUrl("http://github.com/a"))
        assertFalse(ReleaseManifests.allowedUrl("https://github.com.evil.example/a"))
        assertFalse(ReleaseManifests.allowedUrl("https://user@github.com/a"))
        assertFalse(ReleaseManifests.allowedUrl("https://github.com:8443/a"))
    }
}
