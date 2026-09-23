package com.droidbridge.android.product.release

import java.io.File
import java.security.MessageDigest
import java.net.URI
import java.security.KeyFactory
import java.security.PublicKey
import java.security.Signature
import java.security.SignatureException
import java.security.interfaces.ECPublicKey
import java.security.spec.X509EncodedKeySpec
import java.util.Base64
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.longOrNull

/** The six S-DIST-001 release values compiled into this build. */
sealed interface ReleaseConfig {
    /** Debug builds and unprovisioned sources: no update network request is ever made. */
    data object Unconfigured : ReleaseConfig

    data class Configured(
        val owner: String,
        val repository: String,
        val manifestUrl: String,
        val apkSignerSha256: String,
        val releaseKeyId: String,
        val publicKey: PublicKey,
    ) : ReleaseConfig {
        fun releaseBase(version: String) = "https://github.com/$owner/$repository/releases/download/v$version/"
        fun notesUrl(version: String) = "https://github.com/$owner/$repository/releases/tag/v$version"
    }

    companion object {
        private const val UNCONFIGURED = "UNCONFIGURED"
        private val REPO_PART = Regex("[A-Za-z0-9][A-Za-z0-9._-]{0,99}")
        private val KEY_ID = Regex("[a-z0-9][a-z0-9-]{0,63}")

        /** Any sentinel or malformed value makes the whole configuration unavailable. */
        fun from(
            owner: String,
            repository: String,
            manifestUrl: String,
            apkSignerSha256: String,
            releaseKeyId: String,
            publicKeyBase64: String,
        ): ReleaseConfig {
            val values = listOf(owner, repository, manifestUrl, apkSignerSha256, releaseKeyId, publicKeyBase64)
            if (values.any { it == UNCONFIGURED }) return Unconfigured
            if (!REPO_PART.matches(owner) || !REPO_PART.matches(repository)) return Unconfigured
            if (manifestUrl != "https://github.com/$owner/$repository/releases/latest/download/release.json") return Unconfigured
            if (!ReleaseManifests.HEX64.matches(apkSignerSha256) || !KEY_ID.matches(releaseKeyId)) return Unconfigured
            val key = runCatching { p256(Base64.getDecoder().decode(publicKeyBase64)) }.getOrNull() ?: return Unconfigured
            return Configured(owner, repository, manifestUrl, apkSignerSha256, releaseKeyId, key)
        }

        internal fun p256(spki: ByteArray): PublicKey {
            val key = KeyFactory.getInstance("EC").generatePublic(X509EncodedKeySpec(spki))
            require(key is ECPublicKey && key.params.curve.field.fieldSize == 256)
            return key
        }
    }
}

data class ReleaseArtifact(val name: String, val url: String, val size: Long, val sha256: String)

/** One S-DIST-004 manifest that passed signature and field-identity validation. */
data class ReleaseManifest(
    val version: String,
    val versionCode: Long,
    val publishedAt: String,
    val apk: ReleaseArtifact,
    val module: ReleaseArtifact,
    val releaseNotesUrl: String,
)

/** The observed stable module fact used by S-UPD-001 classification. */
enum class ModulePresence { Compatible, Absent, Mismatched, Excluded }

sealed interface ReleaseClassification {
    data object UpToDate : ReleaseClassification
    data class ProductUpdate(val manifest: ReleaseManifest) : ReleaseClassification
    data class ModuleRepair(val manifest: ReleaseManifest) : ReleaseClassification
}

class ReleaseRejected(message: String) : Exception(message)

object ReleaseManifests {
    const val MAX_METADATA_BYTES = 256 * 1024
    const val MAX_ARTIFACT_BYTES = 536_870_912L
    val HEX64 = Regex("[0-9a-f]{64}")
    private val HEX40 = Regex("[0-9a-f]{40}")
    private val SEMVER = Regex("(0|[1-9]\\d{0,2})\\.(0|[1-9]\\d{0,2})\\.(0|[1-9]\\d{0,2})")
    private val PUBLISHED_AT = Regex("\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}Z")

    /** S-DIST-001: every redirect hop and the final host must be HTTPS on exactly these hosts. */
    val ALLOWED_HOSTS = setOf(
        "github.com",
        "api.github.com",
        "objects.githubusercontent.com",
        "release-assets.githubusercontent.com",
    )

    private val MANIFEST_KEYS = setOf(
        "schema_version", "channel", "version", "version_code", "published_at", "min_android_sdk",
        "protocol_version", "store_schema_version", "provenance", "artifacts", "release_notes_url",
    )
    private val TOOLS = mapOf(
        "jdk" to "17", "agp" to "9.4.0", "gradle" to "9.6.0", "kotlin" to "2.4.10", "ndk" to "29.0.14206865",
        "cmake" to "3.31.6", "ninja" to "1.12.1", "winflexbison" to "2.5.25", "rust" to "1.98.0", "cargo_ndk" to "4.1.2",
    )
    private val PROVENANCE_KEYS = setOf("source_revision") + TOOLS.keys + setOf(
        "libpcap_source_sha256", "libpcap_patch_sha256", "gradle_lock_sha256", "gradle_verification_sha256",
        "cargo_lock_sha256", "apk_signer_sha256", "release_key_id",
    )
    private val ARTIFACT_KEYS = setOf("name", "url", "size", "sha256")
    private const val LIBPCAP_SOURCE_SHA256 = "872dd11337fe1ab02ad9d4fee047c9da244d695c6ddf34e2ebb733efd4ed8aa9"

    fun allowedUrl(url: String): Boolean = runCatching {
        val uri = URI(url)
        uri.scheme == "https" && uri.host in ALLOWED_HOSTS && uri.userInfo == null && (uri.port == -1 || uri.port == 443)
    }.getOrDefault(false)

    fun versionCode(version: String): Long {
        val match = SEMVER.matchEntire(version) ?: throw ReleaseRejected("version is not stable SemVer")
        val (major, minor, patch) = match.destructured
        return major.toLong() * 1_000_000 + minor.toLong() * 1_000 + patch.toLong()
    }

    /** Verifies the detached signature over the exact bytes, then validates every field by identity. */
    fun verify(config: ReleaseConfig.Configured, manifestBytes: ByteArray, signature: ByteArray): ReleaseManifest {
        if (manifestBytes.size > MAX_METADATA_BYTES || signature.size > MAX_METADATA_BYTES) {
            throw ReleaseRejected("release metadata exceeds 256 KiB")
        }
        val verifier = Signature.getInstance("SHA256withECDSA")
        verifier.initVerify(config.publicKey)
        verifier.update(manifestBytes)
        val valid = try {
            verifier.verify(signature)
        } catch (_: SignatureException) {
            false
        }
        if (!valid) throw ReleaseRejected("release manifest signature does not verify")
        val text = runCatching { Charsets.UTF_8.newDecoder().decode(java.nio.ByteBuffer.wrap(manifestBytes)).toString() }
            .getOrElse { throw ReleaseRejected("release manifest is not UTF-8") }
        val root = runCatching { Json.parseToJsonElement(text) }.getOrElse { throw ReleaseRejected("release manifest is not JSON") }
        return parse(config, root)
    }

    private fun parse(config: ReleaseConfig.Configured, root: JsonElement): ReleaseManifest {
        val manifest = root.exactObject(MANIFEST_KEYS, "manifest")
        manifest.require("schema_version", manifest.long("schema_version") == 1L)
        manifest.require("channel", manifest.string("channel") == "stable")
        val version = manifest.string("version")
        val versionCode = manifest.long("version_code")
        manifest.require("version_code", versionCode > 0 && versionCode == versionCode(version))
        val publishedAt = manifest.string("published_at")
        manifest.require("published_at", PUBLISHED_AT.matches(publishedAt))
        manifest.require("min_android_sdk", manifest.long("min_android_sdk") == 33L)
        manifest.require("protocol_version", manifest.long("protocol_version") == 1L)
        manifest.require("store_schema_version", manifest.long("store_schema_version") == 1L)
        val notes = manifest.string("release_notes_url")
        manifest.require("release_notes_url", notes == config.notesUrl(version))

        val provenance = manifest.getValue("provenance").exactObject(PROVENANCE_KEYS, "provenance")
        provenance.require("source_revision", HEX40.matches(provenance.string("source_revision")))
        TOOLS.forEach { (key, value) -> provenance.require(key, provenance.string(key) == value) }
        provenance.require("libpcap_source_sha256", provenance.string("libpcap_source_sha256") == LIBPCAP_SOURCE_SHA256)
        listOf("libpcap_patch_sha256", "gradle_lock_sha256", "gradle_verification_sha256", "cargo_lock_sha256").forEach { key ->
            provenance.require(key, HEX64.matches(provenance.string(key)))
        }
        provenance.require("apk_signer_sha256", provenance.string("apk_signer_sha256") == config.apkSignerSha256)
        provenance.require("release_key_id", provenance.string("release_key_id") == config.releaseKeyId)

        val artifacts = manifest.getValue("artifacts").exactObject(setOf("apk", "magisk"), "artifacts")
        val base = config.releaseBase(version)
        return ReleaseManifest(
            version = version,
            versionCode = versionCode,
            publishedAt = publishedAt,
            apk = artifact(artifacts.getValue("apk"), "droidbridge-$version-arm64-v8a.apk", base),
            module = artifact(artifacts.getValue("magisk"), "droidbridge-magisk-$version.zip", base),
            releaseNotesUrl = notes,
        )
    }

    private fun artifact(value: JsonElement, name: String, base: String): ReleaseArtifact {
        val artifact = value.exactObject(ARTIFACT_KEYS, name)
        artifact.require("name", artifact.string("name") == name)
        val url = artifact.string("url")
        artifact.require("url", url == base + name && allowedUrl(url))
        val size = artifact.long("size")
        artifact.require("size", size in 1..MAX_ARTIFACT_BYTES)
        val sha256 = artifact.string("sha256")
        artifact.require("sha256", HEX64.matches(sha256))
        return ReleaseArtifact(name, url, size, sha256)
    }

    /**
     * S-UPD-001/R-UPD-004: lower is rejected; higher is a product update; equal only offers the
     * matching module artifact when the observed module is absent, mismatched or excluded.
     */
    fun classify(manifest: ReleaseManifest, installedVersionCode: Long, module: ModulePresence): ReleaseClassification = when {
        manifest.versionCode < installedVersionCode -> throw ReleaseRejected("signed release is older than the installed APK")
        manifest.versionCode > installedVersionCode -> ReleaseClassification.ProductUpdate(manifest)
        module == ModulePresence.Compatible -> ReleaseClassification.UpToDate
        else -> ReleaseClassification.ModuleRepair(manifest)
    }

    private fun JsonElement.exactObject(keys: Set<String>, what: String): JsonObject {
        val value = this as? JsonObject ?: throw ReleaseRejected("$what is not an object")
        if (value.keys != keys) throw ReleaseRejected("$what fields are not exactly $keys")
        return value
    }

    private fun JsonObject.string(key: String): String {
        val value = get(key) as? JsonPrimitive
        if (value == null || !value.isString) throw ReleaseRejected("$key is not a string")
        return value.content
    }

    private fun JsonObject.long(key: String): Long {
        val value = get(key) as? JsonPrimitive
        val number = value?.takeUnless { it.isString }?.longOrNull
        if (number == null || value.content != number.toString()) throw ReleaseRejected("$key is not an integer")
        return number
    }

    private fun JsonObject.require(key: String, condition: Boolean) {
        if (!condition) throw ReleaseRejected("$key is inconsistent")
    }
}

/** The SHA-256 a release artifact is checked against, wherever it is checked. */
object ReleaseHash {
        fun sha256(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buffer = ByteArray(64 * 1024)
            while (true) {
                val read = input.read(buffer)
                if (read < 0) break
                digest.update(buffer, 0, read)
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }
}
