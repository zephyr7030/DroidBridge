package com.droidbridge.android.execution.android

import android.content.ContentResolver
import android.database.Cursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import java.io.FileNotFoundException
import java.time.Instant
import java.time.format.DateTimeFormatter
import java.time.format.DateTimeFormatterBuilder
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import kotlinx.serialization.json.put

internal data class ContentInspectionEntry(
    val name: String,
    val type: String,
    val size: Long?,
    val modifiedAt: String?,
)

internal data class ContentInspection(
    val type: String,
    val size: Long?,
    val modifiedAt: String?,
    val entries: List<ContentInspectionEntry>?,
    val truncated: Boolean?,
)

internal data class OpenedContent(
    val descriptor: ParcelFileDescriptor,
    val totalSize: Long?,
)

internal interface ContentResolverAccess {
    fun inspect(uri: String, recursive: Boolean, maxDepth: Int, maxEntries: Int): ContentInspection
    fun openRead(uri: String): OpenedContent
}

internal class ContentResolverFilesystemAdapter(
    private val access: ContentResolverAccess,
    private val validatesFence: (String, Long, String) -> Boolean,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException("STALE_AUTHORITY")
        }
        if (request.payload.size > MAX_PAYLOAD_BYTES) {
            throw AndroidExecutionException("RESOURCE_LIMIT")
        }
        return try {
            when (request.primitive) {
                AndroidPrimitive.ContentInspect -> inspect(request.payload)
                AndroidPrimitive.ContentOpenRead -> openRead(request.payload)
                else -> throw AndroidExecutionException("UNSUPPORTED")
            }
        } catch (error: AndroidExecutionException) {
            throw error
        } catch (_: IllegalArgumentException) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
    }

    private fun inspect(payload: ByteArray): AndroidExecutionResult {
        val input = parseObject(payload)
        requireKeys(input, setOf("target", "recursive", "max_depth", "max_entries"))
        val target = parseTarget(input.getValue("target").jsonObject)
        val recursive = input.getValue("recursive").jsonPrimitive.boolean
        val maxDepth = input.getValue("max_depth").jsonPrimitive.int
        val maxEntries = input.getValue("max_entries").jsonPrimitive.int
        if (maxDepth !in 1..16 || maxEntries !in 1..5_000 || (!recursive && maxDepth != 1)) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val result = access.inspect(target, recursive, maxDepth, maxEntries)
        val encoded = buildJsonObject {
            put("target", contentTarget(target))
            put("type", result.type)
            result.size?.let { put("size", it) }
            result.modifiedAt?.let { put("modified_at", it) }
            result.entries?.let { entries ->
                put("entries", buildJsonArray {
                    entries.forEach { entry ->
                        add(buildJsonObject {
                            put("name", entry.name)
                            put("type", entry.type)
                            entry.size?.let { put("size", it) }
                            entry.modifiedAt?.let { put("modified_at", it) }
                        })
                    }
                })
            }
            result.truncated?.let { put("truncated", it) }
        }
        return AndroidExecutionResult(encoded.toString().encodeToByteArray())
    }

    private fun openRead(payload: ByteArray): AndroidExecutionResult {
        val target = parseTarget(parseObject(payload))
        val opened = access.openRead(target)
        val metadata = buildJsonObject {
            opened.totalSize?.let { put("total_size", it) }
        }
        return AndroidExecutionResult(
            metadata.toString().encodeToByteArray(),
            listOf(RoleDescriptor(CONTENT_READ_ROLE, opened.descriptor)),
        )
    }

    private fun parseObject(payload: ByteArray): JsonObject = try {
        Json.parseToJsonElement(payload.decodeToString(throwOnInvalidSequence = true)).jsonObject
    } catch (_: RuntimeException) {
        throw AndroidExecutionException("INVALID_ARGUMENT")
    }

    private fun parseTarget(target: JsonObject): String {
        requireKeys(target, setOf("type", "value"))
        if (target.getValue("type").jsonPrimitive.content != "content_uri") {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        val value = target.getValue("value").jsonPrimitive.content
        val authority = value.removePrefix("content://").substringBefore('/').substringBefore('?').substringBefore('#')
        if (!value.startsWith("content://") || authority.isEmpty() || value.contains('\u0000') ||
            value.encodeToByteArray().size > MAX_URI_BYTES
        ) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        return value
    }

    private fun requireKeys(value: JsonObject, expected: Set<String>) {
        if (value.keys != expected) throw AndroidExecutionException("INVALID_ARGUMENT")
    }

    private fun contentTarget(uri: String) = buildJsonObject {
        put("type", "content_uri")
        put("value", uri)
    }

    private companion object {
        const val CONTENT_READ_ROLE = "content"
        const val MAX_PAYLOAD_BYTES = 1_048_576
        const val MAX_URI_BYTES = 4_096
    }
}

internal class AndroidContentResolverAccess(
    private val resolver: ContentResolver,
) : ContentResolverAccess {
    override fun inspect(
        uri: String,
        recursive: Boolean,
        maxDepth: Int,
        maxEntries: Int,
    ): ContentInspection = translateErrors {
        val parsed = requireContentUri(uri)
        val metadata = queryTargetMetadata(parsed)
        if (metadata.type != TYPE_DIRECTORY) {
            return@translateErrors ContentInspection(
                metadata.type,
                metadata.size,
                metadata.modifiedAt,
                null,
                null,
            )
        }
        if (!DocumentsContract.isTreeUri(parsed)) {
            throw AndroidExecutionException("UNSUPPORTED")
        }
        val entries = mutableListOf<ContentInspectionEntry>()
        var truncated = false

        fun visit(parent: Uri, prefix: String, depth: Int) {
            if (truncated || depth > maxDepth) return
            val documentId = DocumentsContract.getDocumentId(parent)
            val children = DocumentsContract.buildChildDocumentsUriUsingTree(parent, documentId)
            resolver.query(children, DOCUMENT_PROJECTION, null, null, null)?.use { cursor ->
                while (cursor.moveToNext()) {
                    if (entries.size == maxEntries) {
                        truncated = true
                        return@use
                    }
                    val child = documentMetadata(cursor)
                    val childName = safeDocumentName(child.name)
                    val name = if (prefix.isEmpty()) childName else "$prefix/$childName"
                    entries += ContentInspectionEntry(name, child.type, child.size, child.modifiedAt)
                    if (recursive && child.type == TYPE_DIRECTORY && depth < maxDepth) {
                        val childUri = DocumentsContract.buildDocumentUriUsingTree(
                            parsed,
                            child.documentId ?: throw AndroidExecutionException("IO_ERROR"),
                        )
                        visit(childUri, name, depth + 1)
                        if (truncated) return@use
                    }
                }
            } ?: throw AndroidExecutionException("NOT_FOUND")
        }

        visit(treeDocumentUri(parsed), "", 1)
        ContentInspection(TYPE_DIRECTORY, metadata.size, metadata.modifiedAt, entries, truncated)
    }

    override fun openRead(uri: String): OpenedContent = translateErrors {
        val parsed = requireContentUri(uri)
        val descriptor = resolver.openFileDescriptor(parsed, "r")
            ?: throw AndroidExecutionException("NOT_FOUND")
        val size = descriptor.statSize.takeIf { it >= 0 }
            ?: runCatching { queryTargetMetadata(parsed).size }.getOrNull()
        OpenedContent(descriptor, size)
    }

    private fun queryTargetMetadata(uri: Uri): ContentMetadata = if (DocumentsContract.isTreeUri(uri)) {
        resolver.query(treeDocumentUri(uri), DOCUMENT_PROJECTION, null, null, null)?.use { cursor ->
            if (!cursor.moveToFirst()) throw AndroidExecutionException("NOT_FOUND")
            documentMetadata(cursor)
        } ?: throw AndroidExecutionException("NOT_FOUND")
    } else {
        resolver.query(uri, OPENABLE_PROJECTION, null, null, null)?.use { cursor ->
            if (!cursor.moveToFirst()) throw AndroidExecutionException("NOT_FOUND")
            ContentMetadata(
                documentId = null,
                name = cursor.optionalString(OpenableColumns.DISPLAY_NAME)
                    ?: uri.lastPathSegment.orEmpty(),
                type = if (resolver.getType(uri) == DocumentsContract.Document.MIME_TYPE_DIR) {
                    TYPE_DIRECTORY
                } else {
                    TYPE_FILE
                },
                size = cursor.optionalLong(OpenableColumns.SIZE),
                modifiedAt = null,
            )
        } ?: throw AndroidExecutionException("NOT_FOUND")
    }

    private fun documentMetadata(cursor: Cursor): ContentMetadata {
        val mime = cursor.optionalString(DocumentsContract.Document.COLUMN_MIME_TYPE)
        return ContentMetadata(
            documentId = cursor.optionalString(DocumentsContract.Document.COLUMN_DOCUMENT_ID),
            name = cursor.optionalString(OpenableColumns.DISPLAY_NAME).orEmpty(),
            type = if (mime == DocumentsContract.Document.MIME_TYPE_DIR) TYPE_DIRECTORY else TYPE_FILE,
            size = cursor.optionalLong(OpenableColumns.SIZE),
            modifiedAt = cursor.optionalLong(DocumentsContract.Document.COLUMN_LAST_MODIFIED)
                ?.takeIf { it >= 0 }
                ?.let(::formatContentModifiedAt),
        )
    }

    private fun treeDocumentUri(uri: Uri): Uri {
        val documentId = runCatching { DocumentsContract.getDocumentId(uri) }
            .getOrElse { DocumentsContract.getTreeDocumentId(uri) }
        return DocumentsContract.buildDocumentUriUsingTree(uri, documentId)
    }

    private fun safeDocumentName(value: String): String {
        if (value.isEmpty() || value == "." || value == ".." || value.contains('/') ||
            value.contains('\u0000') || value.encodeToByteArray().size > MAX_DOCUMENT_NAME_BYTES
        ) {
            throw AndroidExecutionException("IO_ERROR")
        }
        return value
    }

    private fun Cursor.optionalString(column: String): String? {
        val index = getColumnIndex(column)
        return if (index < 0 || isNull(index)) null else getString(index)
    }

    private fun Cursor.optionalLong(column: String): Long? {
        val index = getColumnIndex(column)
        return if (index < 0 || isNull(index)) null else getLong(index).takeIf { it >= 0 }
    }

    private fun requireContentUri(value: String): Uri {
        val uri = Uri.parse(value)
        if (uri.scheme != ContentResolver.SCHEME_CONTENT || uri.authority.isNullOrEmpty()) {
            throw AndroidExecutionException("INVALID_ARGUMENT")
        }
        return uri
    }

    private fun <T> translateErrors(block: () -> T): T = try {
        block()
    } catch (error: AndroidExecutionException) {
        throw error
    } catch (_: FileNotFoundException) {
        throw AndroidExecutionException("NOT_FOUND")
    } catch (_: SecurityException) {
        throw AndroidExecutionException("PERMISSION_DENIED")
    } catch (_: UnsupportedOperationException) {
        throw AndroidExecutionException("UNSUPPORTED")
    } catch (_: RuntimeException) {
        throw AndroidExecutionException("IO_ERROR")
    }

    private data class ContentMetadata(
        val documentId: String?,
        val name: String,
        val type: String,
        val size: Long?,
        val modifiedAt: String?,
    )

    private companion object {
        const val TYPE_FILE = "file"
        const val TYPE_DIRECTORY = "directory"
        const val MAX_DOCUMENT_NAME_BYTES = 4_096
        val DOCUMENT_PROJECTION = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            OpenableColumns.DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            OpenableColumns.SIZE,
            DocumentsContract.Document.COLUMN_LAST_MODIFIED,
        )
        val OPENABLE_PROJECTION = arrayOf(
            OpenableColumns.DISPLAY_NAME,
            OpenableColumns.SIZE,
        )
    }
}

private val RFC3339_MILLIS: DateTimeFormatter =
    DateTimeFormatterBuilder().appendInstant(3).toFormatter()

internal fun formatContentModifiedAt(epochMillis: Long): String =
    RFC3339_MILLIS.format(Instant.ofEpochMilli(epochMillis))
