package com.droidbridge.android.execution.shizuku

import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction
import java.util.Base64
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.longOrNull

internal sealed interface ShizukuFsOperation {
    data class Lstat(val path: String) : ShizukuFsOperation
    data class AccessWriteSearch(val path: String) : ShizukuFsOperation
    data class OpenRead(val path: String) : ShizukuFsOperation

    /**
     * Reads at most [limit] bytes of [path] inside the shell-UID process. A descriptor passed to
     * the App is read under the App domain, which Android policy refuses for some procfs files.
     */
    data class ReadBounded(val path: String, val limit: Int) : ShizukuFsOperation
    data class ReadDirectory(val path: String, val cookie: Long, val limit: Int) : ShizukuFsOperation
    data class CreateExclusive(val path: String, val mode: Int) : ShizukuFsOperation
    data class OpenWrite(val path: String) : ShizukuFsOperation
    data object FsyncDescriptor : ShizukuFsOperation
    data class ApplyMetadata(
        val uid: Int,
        val gid: Int,
        val mode: Int,
        val selinuxContext: ByteArray?,
    ) : ShizukuFsOperation
    data class RenameAtomic(
        val source: String,
        val destination: String,
        val exchange: Boolean,
    ) : ShizukuFsOperation
    data class FsyncDirectory(val path: String) : ShizukuFsOperation
    data class Mkdir(val path: String, val mode: Int) : ShizukuFsOperation
    data class Unlink(val path: String) : ShizukuFsOperation
    data class Readlink(val path: String) : ShizukuFsOperation
    data class Symlink(val target: String, val destination: String) : ShizukuFsOperation
}

internal object ShizukuFsCodec {
    fun decode(payload: ByteArray): ShizukuFsOperation {
        require(payload.size <= MAX_PAYLOAD_BYTES)
        val value = Json.parseToJsonElement(strictUtf8(payload)) as? JsonObject
            ?: throw IllegalArgumentException("filesystem payload must be an object")
        return when (value.requiredString("operation")) {
            "lstat" -> ShizukuFsOperation.Lstat(value.pathOnly("path"))
            "access_write_search" -> ShizukuFsOperation.AccessWriteSearch(value.pathOnly("path"))
            "open_read" -> ShizukuFsOperation.OpenRead(value.pathOnly("path"))
            "read_bounded" -> {
                requireKeys(value, "operation", "path", "limit")
                val limit = value.requiredInt("limit")
                require(limit in 1..MAX_READ_BOUNDED_BYTES)
                ShizukuFsOperation.ReadBounded(value.requiredPath("path"), limit)
            }
            "read_directory" -> {
                requireKeys(value, "operation", "path", "cookie", "limit")
                val cookie = value.requiredLong("cookie")
                val limit = value.requiredInt("limit")
                require(cookie >= 0 && limit in 1..MAX_DIRECTORY_PAGE_ENTRIES)
                ShizukuFsOperation.ReadDirectory(value.requiredPath("path"), cookie, limit)
            }
            "create_exclusive" -> {
                requireKeys(value, "operation", "path", "mode")
                ShizukuFsOperation.CreateExclusive(value.requiredPath("path"), value.requiredMode())
            }
            "open_write" -> ShizukuFsOperation.OpenWrite(value.pathOnly("path"))
            "fsync_descriptor" -> {
                requireKeys(value, "operation")
                ShizukuFsOperation.FsyncDescriptor
            }
            "apply_metadata" -> {
                requireKeys(value, "operation", "uid", "gid", "mode", "selinux_context_base64")
                val uid = value.requiredInt("uid")
                val gid = value.requiredInt("gid")
                require(uid >= 0 && gid >= 0)
                val context = Base64.getDecoder().decode(value.requiredString("selinux_context_base64"))
                require(context.size <= 4_096)
                require(context.isEmpty() || context.last() == 0.toByte())
                ShizukuFsOperation.ApplyMetadata(
                    uid,
                    gid,
                    value.requiredMode(),
                    context.takeUnless(ByteArray::isEmpty),
                )
            }
            "rename_atomic" -> {
                requireKeys(value, "operation", "source", "destination", "exchange")
                val source = value.requiredPath("source")
                val destination = value.requiredPath("destination")
                ShizukuFsOperation.RenameAtomic(
                    source,
                    destination,
                    value.requiredBoolean("exchange"),
                )
            }
            "fsync_directory" -> ShizukuFsOperation.FsyncDirectory(value.pathOnly("path"))
            "mkdir" -> {
                requireKeys(value, "operation", "path", "mode")
                ShizukuFsOperation.Mkdir(value.requiredPath("path"), value.requiredMode())
            }
            "unlink" -> ShizukuFsOperation.Unlink(value.pathOnly("path"))
            "readlink" -> ShizukuFsOperation.Readlink(value.pathOnly("path"))
            "symlink" -> {
                requireKeys(value, "operation", "target", "destination")
                val target = value.requiredString("target")
                require(target.isNotEmpty() && '\u0000' !in target)
                require(target.toByteArray(Charsets.UTF_8).size <= 4_096)
                ShizukuFsOperation.Symlink(target, value.requiredPath("destination"))
            }
            else -> throw IllegalArgumentException("unknown filesystem primitive")
        }
    }

    private fun JsonObject.pathOnly(key: String): String {
        requireKeys(this, "operation", key)
        return requiredPath(key)
    }

    private fun JsonObject.requiredPath(key: String): String = requiredString(key).also { path ->
        require(path.startsWith('/') && '\u0000' !in path)
        require(path.toByteArray(Charsets.UTF_8).size <= 4_096)
        if (path != "/") {
            require("//" !in path && !path.endsWith('/'))
            require(path.split('/').drop(1).none { it == "." || it == ".." || it.isEmpty() })
        }
    }

    private fun JsonObject.requiredMode(): Int = requiredInt("mode").also { mode ->
        require(mode in 0..4_095)
    }

    private fun JsonObject.requiredString(key: String): String =
        (this[key] as? JsonPrimitive)
            ?.takeIf { it.isString }
            ?.content
            ?: throw IllegalArgumentException("missing string")

    private fun JsonObject.requiredInt(key: String): Int =
        (this[key] as? JsonPrimitive)
            ?.takeIf { !it.isString }
            ?.intOrNull
            ?: throw IllegalArgumentException("missing integer")

    private fun JsonObject.requiredLong(key: String): Long =
        (this[key] as? JsonPrimitive)
            ?.takeIf { !it.isString }
            ?.longOrNull
            ?: throw IllegalArgumentException("missing integer")

    private fun JsonObject.requiredBoolean(key: String): Boolean =
        (this[key] as? JsonPrimitive)
            ?.takeIf { !it.isString }
            ?.content
            ?.let { value ->
                when (value) {
                    "true" -> true
                    "false" -> false
                    else -> null
                }
            }
            ?: throw IllegalArgumentException("missing boolean")

    private fun requireKeys(value: JsonObject, vararg expected: String) {
        require(value.keys == expected.toSet())
    }

    private fun strictUtf8(payload: ByteArray): String = try {
        Charsets.UTF_8.newDecoder()
            .onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
            .decode(ByteBuffer.wrap(payload))
            .toString()
    } catch (error: CharacterCodingException) {
        throw IllegalArgumentException("invalid UTF-8", error)
    }

    private const val MAX_PAYLOAD_BYTES = 65_536
    private const val MAX_DIRECTORY_PAGE_ENTRIES = 5_001

    /** The largest bounded read, sized so its encoded reply stays well inside one binder call. */
    const val MAX_READ_BOUNDED_BYTES = 262_144
}
