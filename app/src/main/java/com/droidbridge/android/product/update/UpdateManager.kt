package com.droidbridge.android.product.update

import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.URI
import java.net.URL
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.MessageDigest
import javax.net.ssl.HttpsURLConnection
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.withContext

/** One bounded HTTPS read; implementations enforce the S-DIST-001 host policy on every hop. */
interface ReleaseTransport {
    fun <T> get(url: String, maxBytes: Long, read: (InputStream) -> T): T
}

/** Platform HttpsURLConnection with manual, allow-listed redirects and no retry (S-UPD-001). */
object HttpsReleaseTransport : ReleaseTransport {
    private const val CONNECT_TIMEOUT_MILLIS = 15_000
    private const val READ_TIMEOUT_MILLIS = 30_000
    private const val MAX_REDIRECTS = 5

    override fun <T> get(url: String, maxBytes: Long, read: (InputStream) -> T): T {
        var current = url
        repeat(MAX_REDIRECTS + 1) {
            if (!ReleaseManifests.allowedUrl(current)) throw ReleaseRejected("release host is not allowed")
            val connection = URL(current).openConnection() as HttpsURLConnection
            try {
                connection.instanceFollowRedirects = false
                connection.connectTimeout = CONNECT_TIMEOUT_MILLIS
                connection.readTimeout = READ_TIMEOUT_MILLIS
                connection.useCaches = false
                when (val code = connection.responseCode) {
                    HttpsURLConnection.HTTP_OK -> {
                        val declared = connection.contentLengthLong
                        if (declared > maxBytes) throw ReleaseRejected("release download exceeds its bound")
                        return connection.inputStream.use { stream -> read(BoundedInputStream(stream, maxBytes)) }
                    }
                    in 301..308 -> {
                        val location = connection.getHeaderField("Location") ?: throw IOException("redirect without Location")
                        current = URI(current).resolve(location).toString()
                    }
                    else -> throw IOException("release server returned HTTP $code")
                }
            } finally {
                connection.disconnect()
            }
        }
        throw IOException("too many release redirects")
    }
}

private class BoundedInputStream(private val delegate: InputStream, private val limit: Long) : InputStream() {
    private var count = 0L

    override fun read(): Int = delegate.read().also { if (it >= 0) consumed(1) }

    override fun read(buffer: ByteArray, offset: Int, length: Int): Int =
        delegate.read(buffer, offset, length).also { if (it > 0) consumed(it.toLong()) }

    private fun consumed(bytes: Long) {
        count += bytes
        if (count > limit) throw ReleaseRejected("release download exceeds its bound")
    }

    override fun close() = delegate.close()
}

sealed interface UpdateCheck {
    data object Unconfigured : UpdateCheck
    data object Idle : UpdateCheck
    data object Checking : UpdateCheck
    data object Failed : UpdateCheck
    /** A verified classification plus the exact signed bytes the Runtime re-verifies before maintenance. */
    class Checked(val classification: ReleaseClassification, val manifest: ByteArray, val signature: ByteArray) : UpdateCheck
}

/** Artifacts already verified against the signed manifest and renamed into the update cache. */
data class VerifiedDownloads(val manifest: ReleaseManifest, val apk: File?, val module: File?)

data class UpdateState(
    val check: UpdateCheck,
    val downloading: Boolean = false,
    val downloadFailed: Boolean = false,
    val downloads: VerifiedDownloads? = null,
) {
    /** The `ui.product.v1` Home slot input: a completed verified check found a newer release. */
    val newerVersionAvailable: Boolean
        get() = (check as? UpdateCheck.Checked)?.classification is ReleaseClassification.ProductUpdate
}

/**
 * The default-process S-UPD-001 update state machine. It never writes maintenance authority;
 * installation starts only from the Runtime HostController after an explicit user action.
 */
class UpdateManager(
    private val config: ReleaseConfig,
    private val installedVersionCode: Long,
    private val cacheRoot: File,
    private val transport: ReleaseTransport = HttpsReleaseTransport,
    private val clock: () -> Long = System::currentTimeMillis,
) {
    private val mutableState = MutableStateFlow(
        UpdateState(if (config is ReleaseConfig.Configured) UpdateCheck.Idle else UpdateCheck.Unconfigured),
    )
    val state: StateFlow<UpdateState> = mutableState.asStateFlow()

    suspend fun check(module: ModulePresence) {
        val configured = config as? ReleaseConfig.Configured ?: return
        mutableState.update { it.copy(check = UpdateCheck.Checking, downloadFailed = false) }
        val result = withContext(Dispatchers.IO) {
            runCatching {
                val bytes = transport.get(configured.manifestUrl, ReleaseManifests.MAX_METADATA_BYTES.toLong()) { it.readBytes() }
                val signature = transport.get(configured.manifestUrl + ".sig", ReleaseManifests.MAX_METADATA_BYTES.toLong()) { it.readBytes() }
                UpdateCheck.Checked(
                    ReleaseManifests.classify(ReleaseManifests.verify(configured, bytes, signature), installedVersionCode, module),
                    bytes,
                    signature,
                )
            }
        }
        mutableState.update { current ->
            val checked = result.getOrNull()
            val downloads = current.downloads?.takeIf { prior ->
                checked?.classification.manifestOrNull()?.let { it == prior.manifest } == true
            }
            current.copy(
                check = checked ?: UpdateCheck.Failed,
                downloads = downloads,
            )
        }
    }

    /**
     * Downloads the artifacts the verified classification needs: a product update takes the APK
     * and, when a module is part of this device, the module ZIP; a repair takes only the module.
     */
    suspend fun download(moduleRequired: Boolean) {
        val checked = (mutableState.value.check as? UpdateCheck.Checked)?.classification ?: return
        val (manifest, wantApk, wantModule) = when (checked) {
            is ReleaseClassification.ProductUpdate -> Triple(checked.manifest, true, moduleRequired)
            is ReleaseClassification.ModuleRepair -> Triple(checked.manifest, false, true)
            ReleaseClassification.UpToDate -> return
        }
        mutableState.update { it.copy(downloading = true, downloadFailed = false) }
        val result = withContext(Dispatchers.IO) {
            runCatching {
                VerifiedDownloads(
                    manifest = manifest,
                    apk = if (wantApk) fetchVerified(manifest.version, manifest.apk) else null,
                    module = if (wantModule) fetchVerified(manifest.version, manifest.module) else null,
                )
            }
        }
        mutableState.update { current ->
            current.copy(downloading = false, downloadFailed = result.isFailure, downloads = result.getOrNull() ?: current.downloads)
        }
    }

    private fun fetchVerified(version: String, artifact: ReleaseArtifact): File {
        val directory = File(cacheRoot, version)
        val target = File(directory, artifact.name)
        if (target.isFile && target.length() == artifact.size && sha256(target) == artifact.sha256) return target
        if (!directory.isDirectory && !directory.mkdirs()) throw IOException("update cache is unavailable")
        val part = File(directory, ".${artifact.name}.part")
        part.delete()
        val digest = MessageDigest.getInstance("SHA-256")
        var written = 0L
        transport.get(artifact.url, artifact.size) { input ->
            part.outputStream().use { output ->
                val buffer = ByteArray(64 * 1024)
                while (true) {
                    val read = input.read(buffer)
                    if (read < 0) break
                    digest.update(buffer, 0, read)
                    output.write(buffer, 0, read)
                    written += read
                }
                output.fd.sync()
            }
        }
        val actual = digest.digest().joinToString("") { "%02x".format(it) }
        if (written != artifact.size || actual != artifact.sha256) {
            part.delete()
            throw ReleaseRejected("downloaded ${artifact.name} does not match the signed manifest")
        }
        Files.move(part.toPath(), target.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
        return target
    }

    /**
     * S-UPD-001 cleanup on Updates entry and cold start: files older than 24 hours go first, then
     * unreferenced verified artifacts oldest-first while the cache exceeds 1 GiB.
     */
    fun cleanup(referenced: Set<File>) {
        val now = clock()
        val files = cacheRoot.walkBottomUp().filter { it.isFile }.sortedBy { it.lastModified() }.toList()
        val remaining = files.filterNot { file ->
            (now - file.lastModified() > MAX_AGE_MILLIS && file !in referenced) && file.delete()
        }.toMutableList()
        var total = remaining.sumOf { it.length() }
        remaining.filter { it !in referenced }.forEach { file ->
            if (total <= MAX_CACHE_BYTES) return@forEach
            val size = file.length()
            if (file.delete()) total -= size
        }
        cacheRoot.walkBottomUp().filter { it.isDirectory && it != cacheRoot }.forEach { it.delete() }
        mutableState.update { current ->
            val downloads = current.downloads ?: return@update current
            val apk = downloads.apk?.takeIf(File::isFile)
            val module = downloads.module?.takeIf(File::isFile)
            current.copy(downloads = if (apk == null && module == null) null else downloads.copy(apk = apk, module = module))
        }
    }

    private fun ReleaseClassification?.manifestOrNull(): ReleaseManifest? = when (this) {
        is ReleaseClassification.ProductUpdate -> manifest
        is ReleaseClassification.ModuleRepair -> manifest
        else -> null
    }

    companion object {
        private const val MAX_AGE_MILLIS = 24L * 60 * 60 * 1000
        private const val MAX_CACHE_BYTES = 1L shl 30

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
}
