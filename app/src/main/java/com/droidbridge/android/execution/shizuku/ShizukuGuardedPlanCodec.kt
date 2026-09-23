package com.droidbridge.android.execution.shizuku

import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.intOrNull

internal data class ShizukuGuardedPlan(
    val program: String,
    val arguments: List<String>,
    val cwd: String,
    val deadlineMs: Long,
    val stdoutLimit: Int,
)

internal data class ShizukuGuardedInvocation(
    val primitive: String,
    val payload: ByteArray,
    val plan: ShizukuGuardedPlan,
)

internal object ShizukuGuardedPlanCodec {
    fun decodeInvocation(
        payload: ByteArray,
        guardPath: String,
        currentPackage: String? = null,
    ): ShizukuGuardedInvocation {
        require(payload.size <= MAX_PAYLOAD_BYTES)
        val value = Json.parseToJsonElement(strictUtf8(payload)) as? JsonObject
            ?: throw IllegalArgumentException("primitive invocation must be an object")
        val primitive = value.requiredString("operation")
        val inner = JsonObject(value.filterKeys { it != "operation" }).toString().toByteArray()
        return ShizukuGuardedInvocation(
            primitive,
            inner,
            decode(primitive, inner, guardPath, currentPackage),
        )
    }

    fun decode(
        primitive: String,
        payload: ByteArray,
        guardPath: String,
        currentPackage: String? = null,
    ): ShizukuGuardedPlan {
        require(payload.size <= MAX_PAYLOAD_BYTES)
        val objectValue = Json.parseToJsonElement(strictUtf8(payload)) as? JsonObject
            ?: throw IllegalArgumentException("primitive payload must be an object")
        return when (primitive) {
            "guard_probe" -> {
                requireKeys(objectValue)
                ShizukuGuardedPlan(guardPath, listOf("--probe-child"), "/", 5_000, 8_192)
            }
            "process_start" -> processStart(objectValue)
            "screen_capture" -> {
                requireKeys(objectValue)
                ShizukuGuardedPlan("/system/bin/screencap", listOf("-p"), "/", 5_000, MAX_OUTPUT_BYTES)
            }
            "input_tap" -> inputTap(objectValue)
            "input_long_press" -> inputLongPress(objectValue)
            "input_swipe" -> inputSwipe(objectValue)
            "input_key" -> inputKey(objectValue)
            "input_key_combination" -> inputKeyCombination(objectValue)
            "input_text" -> inputText(objectValue)
            "uiautomator_dump" -> uiAutomatorDump(objectValue)
            "package_list_third_party" -> packageList(objectValue, system = false)
            "package_list_system" -> packageList(objectValue, system = true)
            "package_force_stop" -> packageForceStop(objectValue, currentPackage)
            else -> throw IllegalArgumentException("unknown Shizuku primitive")
        }
    }

    private fun processStart(value: JsonObject): ShizukuGuardedPlan {
        requireKeys(value, "program", "arguments", "timeout_ms", "cwd", "max_output_bytes")
        val program = value.requiredString("program")
        require(program == COMMAND_PROGRAM)
        val arguments = value["arguments"] as? JsonArray
            ?: throw IllegalArgumentException("arguments must be an array")
        require(arguments.size == 2)
        var totalBytes = 0
        val decoded = arguments.map { element ->
            val argument = (element as? JsonPrimitive)
                ?.takeIf { it.isString }
                ?.content
                ?: throw IllegalArgumentException("argument must be a string")
            require('\u0000' !in argument)
            val bytes = argument.toByteArray(Charsets.UTF_8).size
            require(bytes <= COMMAND_MAX_BYTES)
            totalBytes = Math.addExact(totalBytes, bytes)
            argument
        }
        require(decoded[0] == "-c")
        require(decoded[1].isNotEmpty())
        require(totalBytes <= COMMAND_MAX_BYTES + 2)
        val cwd = value.requiredString("cwd")
        requireAbsoluteValue(cwd, 4_096)
        val timeout = value.requiredInt("timeout_ms")
        require(timeout in COMMAND_MIN_TIMEOUT_MS..COMMAND_MAX_TIMEOUT_MS)
        val outputBytes = value.requiredInt("max_output_bytes")
        require(outputBytes in COMMAND_MIN_OUTPUT_BYTES..COMMAND_MAX_OUTPUT_BYTES)
        return ShizukuGuardedPlan(program, decoded, cwd, timeout.toLong(), outputBytes)
    }

    private fun inputTap(value: JsonObject): ShizukuGuardedPlan {
        requireKeys(value, "x", "y")
        val x = value.requiredCoordinate("x")
        val y = value.requiredCoordinate("y")
        return ShizukuGuardedPlan("/system/bin/input", listOf("tap", x, y), "/", 5_000, 8_192)
    }

    private fun inputLongPress(value: JsonObject): ShizukuGuardedPlan {
        requireKeys(value, "x", "y")
        val x = value.requiredCoordinate("x")
        val y = value.requiredCoordinate("y")
        return ShizukuGuardedPlan(
            "/system/bin/input",
            listOf("swipe", x, y, x, y, "500"),
            "/",
            5_000,
            8_192,
        )
    }

    private fun inputSwipe(value: JsonObject): ShizukuGuardedPlan {
        requireKeys(value, "x1", "y1", "x2", "y2", "duration_ms")
        val duration = value.requiredInt("duration_ms")
        require(duration in 1..10_000)
        return ShizukuGuardedPlan(
            "/system/bin/input",
            listOf(
                "swipe",
                value.requiredCoordinate("x1"),
                value.requiredCoordinate("y1"),
                value.requiredCoordinate("x2"),
                value.requiredCoordinate("y2"),
                duration.toString(),
            ),
            "/",
            (duration + 5_000).coerceAtMost(15_000).toLong(),
            8_192,
        )
    }

    private fun inputKey(value: JsonObject): ShizukuGuardedPlan {
        requireKeys(value, "key_code")
        val keyCode = value.requiredInt("key_code")
        require(keyCode >= 0)
        return ShizukuGuardedPlan(
            "/system/bin/input",
            listOf("keyevent", keyCode.toString()),
            "/",
            5_000,
            8_192,
        )
    }

    /** Modifier keys held while the last key is pressed; at most the four modifier pairs plus the key. */
    private fun inputKeyCombination(value: JsonObject): ShizukuGuardedPlan {
        requireKeys(value, "key_codes")
        val keyCodes = (value["key_codes"] as? JsonArray)
            ?.map { element ->
                (element as? JsonPrimitive)?.takeIf { !it.isString }?.intOrNull
                    ?: throw IllegalArgumentException("key code must be an integer")
            }
            ?: throw IllegalArgumentException("key_codes must be an array")
        require(keyCodes.size in 2..9 && keyCodes.all { it >= 0 })
        return ShizukuGuardedPlan(
            "/system/bin/input",
            listOf("keycombination") + keyCodes.map(Int::toString),
            "/",
            5_000,
            8_192,
        )
    }

    private fun inputText(value: JsonObject): ShizukuGuardedPlan {
        requireKeys(value, "text")
        val text = value.requiredString("text")
        require('\u0000' !in text && text.toByteArray(Charsets.UTF_8).size <= 8_192)
        return ShizukuGuardedPlan("/system/bin/input", listOf("text", text), "/", 5_000, 8_192)
    }

    private fun uiAutomatorDump(value: JsonObject): ShizukuGuardedPlan {
        requireKeys(value, "request_temp_path")
        val path = value.requiredString("request_temp_path")
        requireAbsoluteValue(path, 4_096)
        return ShizukuGuardedPlan(
            "/system/bin/uiautomator",
            listOf("dump", path),
            "/",
            15_000,
            MAX_OUTPUT_BYTES,
        )
    }

    private fun packageList(value: JsonObject, system: Boolean): ShizukuGuardedPlan {
        requireKeys(value)
        return ShizukuGuardedPlan(
            "/system/bin/cmd",
            listOf(
                "package",
                "list",
                "packages",
                if (system) "-s" else "-3",
                "--show-versioncode",
                "--user",
                "0",
            ),
            "/",
            15_000,
            MAX_OUTPUT_BYTES,
        )
    }

    private fun packageForceStop(value: JsonObject, currentPackage: String?): ShizukuGuardedPlan {
        requireKeys(value, "package_name")
        val packageName = value.requiredString("package_name")
        requirePackageName(packageName)
        require(packageName != currentPackage)
        return ShizukuGuardedPlan(
            "/system/bin/am",
            listOf("force-stop", "--user", "0", packageName),
            "/",
            15_000,
            8_192,
        )
    }

    private fun requireKeys(value: JsonObject, vararg expected: String) {
        require(value.keys == expected.toSet()) { "primitive payload fields are invalid" }
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

    private fun JsonObject.requiredCoordinate(key: String): String = requiredInt(key).also {
        require(it >= 0)
    }.toString()

    private fun requireAbsoluteValue(value: String, maxBytes: Int) {
        require(value.startsWith('/') && '\u0000' !in value)
        require(value.toByteArray(Charsets.UTF_8).size <= maxBytes)
    }

    private fun requirePackageName(value: String) {
        require(value.isNotEmpty() && '\u0000' !in value)
        require(value.toByteArray(Charsets.UTF_8).size <= 255)
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
    private const val MAX_OUTPUT_BYTES = 8 * 1_024 * 1_024
    private const val COMMAND_PROGRAM = "/system/bin/sh"
    private const val COMMAND_MAX_BYTES = 32_768
    private const val COMMAND_MIN_TIMEOUT_MS = 1_000
    private const val COMMAND_MAX_TIMEOUT_MS = 150_000
    private const val COMMAND_MIN_OUTPUT_BYTES = 1_024
    private const val COMMAND_MAX_OUTPUT_BYTES = 1_024 * 1_024
}
