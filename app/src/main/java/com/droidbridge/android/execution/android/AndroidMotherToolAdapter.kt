package com.droidbridge.android.execution.android

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

internal data class VisiblePackageFact(
    val packageName: String,
    val versionName: String?,
    val versionCode: Long,
    val enabled: Boolean,
    val system: Boolean,
    val launchable: Boolean,
)

/**
 * One explicit/non-broadcast Activity start. Callers never supply flags or categories;
 * the platform access adds only the S-ANDROID-006 non-Activity-context flag.
 */
internal data class ActivityStart(
    val action: String,
    val dataUri: String?,
    val packageName: String?,
    val className: String?,
    val extras: Map<String, Any>,
)

internal interface AndroidMotherToolAccess {
    /** `null` is the PackageManager `NameNotFoundException` visibility-or-absent fact. */
    fun inspectPackage(packageName: String): VisiblePackageFact?

    fun launchPackage(packageName: String)

    fun startActivity(start: ActivityStart)

    /** Returns only direct text of the first primary-clip item, else `null`. */
    fun readClipboardText(): String?

    fun writeClipboardText(text: String)

    fun clearClipboard()
}

/** S-ANDROID-006 package, launch, intent and clipboard framework primitives. */
internal class AndroidMotherToolAdapter(
    private val access: AndroidMotherToolAccess,
    private val validatesFence: (String, Long, String) -> Boolean,
) : AndroidExecutionBridge {
    override suspend fun execute(request: AndroidExecutionRequest): AndroidExecutionResult {
        if (!validatesFence(request.runtimeEpoch, request.hostGeneration, request.runtimeInstanceId)) {
            throw AndroidExecutionException(STALE_AUTHORITY)
        }
        if (request.descriptors.isNotEmpty() || request.payload.size > MAX_PAYLOAD_BYTES) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
        val input = parseObject(request.payload)
        return try {
            when (request.primitive) {
                AndroidPrimitive.PackageInspect -> inspect(input)
                AndroidPrimitive.LaunchActivity -> launch(input)
                AndroidPrimitive.IntentStart -> intent(input)
                AndroidPrimitive.ClipboardRead -> {
                    requireKeys(input, emptySet())
                    val text = access.readClipboardText()
                    AndroidExecutionResult(
                        buildJsonObject { text?.let { put("text", it) } }.toString().encodeToByteArray(),
                    )
                }
                AndroidPrimitive.ClipboardWrite -> {
                    requireKeys(input, setOf("text"))
                    val text = input.string("text")
                    if (text.encodeToByteArray().size > MAX_CLIPBOARD_BYTES) {
                        throw AndroidExecutionException(INVALID_ARGUMENT)
                    }
                    access.writeClipboardText(text)
                    completed()
                }
                AndroidPrimitive.ClipboardClear -> {
                    requireKeys(input, emptySet())
                    access.clearClipboard()
                    completed()
                }
                else -> throw AndroidExecutionException(UNSUPPORTED)
            }
        } catch (_: SecurityException) {
            throw AndroidExecutionException(PERMISSION_DENIED)
        }
    }

    private fun inspect(input: JsonObject): AndroidExecutionResult {
        requireKeys(input, setOf("package_name"))
        val packageName = input.packageName("package_name")
        val fact = access.inspectPackage(packageName)
        val encoded = if (fact == null) {
            buildJsonObject { put("status", "visibility_or_absent") }
        } else {
            if (fact.packageName != packageName) throw AndroidExecutionException(IO_ERROR)
            buildJsonObject {
                put("status", "visible")
                put("package", buildJsonObject {
                    put("package_name", fact.packageName)
                    fact.versionName
                        ?.takeIf { it.encodeToByteArray().size <= MAX_VERSION_NAME_BYTES }
                        ?.let { put("version_name", it) }
                    put("version_code", fact.versionCode)
                    put("enabled", fact.enabled)
                    put("system", fact.system)
                    put("launchable", fact.launchable)
                })
            }
        }
        return AndroidExecutionResult(encoded.toString().encodeToByteArray())
    }

    private fun launch(input: JsonObject): AndroidExecutionResult {
        when (input.string("operation")) {
            "package" -> {
                requireKeys(input, setOf("operation", "package_name"))
                access.launchPackage(input.packageName("package_name"))
            }
            "component" -> {
                requireKeys(input, setOf("operation", "package_name", "class_name"))
                val packageName = input.packageName("package_name")
                access.startActivity(
                    ActivityStart(ACTION_MAIN, null, packageName, input.className(), emptyMap()),
                )
            }
            else -> throw AndroidExecutionException(INVALID_ARGUMENT)
        }
        return completed()
    }

    private fun intent(input: JsonObject): AndroidExecutionResult {
        when (input.string("operation")) {
            "view" -> {
                requireSubset(input, setOf("operation", "data_uri"), setOf("package_name"))
                access.startActivity(
                    ActivityStart(
                        action = ACTION_VIEW,
                        dataUri = input.uri("data_uri"),
                        packageName = input["package_name"]?.let { input.packageName("package_name") },
                        className = null,
                        extras = emptyMap(),
                    ),
                )
            }
            "explicit_activity" -> {
                requireSubset(
                    input,
                    setOf("operation", "package_name", "class_name"),
                    setOf("action", "data_uri", "extras"),
                )
                access.startActivity(
                    ActivityStart(
                        action = input["action"]?.let { input.string("action") } ?: ACTION_MAIN,
                        dataUri = input["data_uri"]?.let { input.uri("data_uri") },
                        packageName = input.packageName("package_name"),
                        className = input.className(),
                        extras = input["extras"]?.let(::extras) ?: emptyMap(),
                    ),
                )
            }
            else -> throw AndroidExecutionException(INVALID_ARGUMENT)
        }
        return completed()
    }

    private fun extras(element: kotlinx.serialization.json.JsonElement): Map<String, Any> {
        val value = element as? JsonObject ?: throw AndroidExecutionException(INVALID_ARGUMENT)
        if (value.size > MAX_EXTRAS) throw AndroidExecutionException(INVALID_ARGUMENT)
        return value.mapValues { (key, extra) ->
            val size = key.encodeToByteArray().size
            if (size !in 1..MAX_EXTRA_KEY_BYTES) throw AndroidExecutionException(INVALID_ARGUMENT)
            val primitive = extra as? JsonPrimitive ?: throw AndroidExecutionException(INVALID_ARGUMENT)
            when {
                primitive.isString -> primitive.content.also {
                    if (it.encodeToByteArray().size > MAX_EXTRA_STRING_BYTES) {
                        throw AndroidExecutionException(INVALID_ARGUMENT)
                    }
                }
                primitive.booleanOrNull != null -> primitive.booleanOrNull!!
                primitive.longOrNull != null -> primitive.longOrNull!!
                else -> throw AndroidExecutionException(INVALID_ARGUMENT)
            }
        }
    }

    private fun completed() = AndroidExecutionResult(COMPLETED)

    private fun parseObject(payload: ByteArray): JsonObject = try {
        Json.parseToJsonElement(payload.decodeToString(throwOnInvalidSequence = true)).jsonObject
    } catch (_: IllegalArgumentException) {
        throw AndroidExecutionException(INVALID_ARGUMENT)
    } catch (_: CharacterCodingException) {
        throw AndroidExecutionException(INVALID_ARGUMENT)
    }

    private fun requireKeys(input: JsonObject, keys: Set<String>) {
        if (input.keys != keys) throw AndroidExecutionException(INVALID_ARGUMENT)
    }

    private fun requireSubset(input: JsonObject, required: Set<String>, optional: Set<String>) {
        if (!input.keys.containsAll(required) || !(required + optional).containsAll(input.keys)) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
    }

    private fun JsonObject.string(key: String): String = this[key]?.jsonPrimitive
        ?.takeIf { it.isString }
        ?.contentOrNull
        ?: throw AndroidExecutionException(INVALID_ARGUMENT)

    private fun JsonObject.packageName(key: String): String = string(key).also {
        if (it.isEmpty() || ' ' in it || it.encodeToByteArray().size > MAX_PACKAGE_NAME_BYTES) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
    }

    private fun JsonObject.className(): String = string("class_name").also {
        if (it.isEmpty() || ' ' in it || it.encodeToByteArray().size > MAX_CLASS_NAME_BYTES) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
    }

    private fun JsonObject.uri(key: String): String = string(key).also {
        if (' ' in it || it.encodeToByteArray().size > MAX_URI_BYTES) {
            throw AndroidExecutionException(INVALID_ARGUMENT)
        }
    }

    private companion object {
        const val ACTION_MAIN = "android.intent.action.MAIN"
        const val ACTION_VIEW = "android.intent.action.VIEW"
        const val MAX_PAYLOAD_BYTES = 262_144
        const val MAX_CLIPBOARD_BYTES = 65_536
        const val MAX_PACKAGE_NAME_BYTES = 255
        const val MAX_CLASS_NAME_BYTES = 512
        const val MAX_URI_BYTES = 4_096
        const val MAX_VERSION_NAME_BYTES = 256
        const val MAX_EXTRAS = 32
        const val MAX_EXTRA_KEY_BYTES = 128
        const val MAX_EXTRA_STRING_BYTES = 4_096
        const val STALE_AUTHORITY = "STALE_AUTHORITY"
        const val INVALID_ARGUMENT = "INVALID_ARGUMENT"
        const val PERMISSION_DENIED = "PERMISSION_DENIED"
        const val IO_ERROR = "IO_ERROR"
        const val UNSUPPORTED = "UNSUPPORTED"
        val COMPLETED = "{\"completed\":true}".encodeToByteArray()
    }
}
