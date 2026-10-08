package com.droidbridge.standalone.runtimehost

import android.system.Os
import android.system.OsConstants
import java.io.File
import java.io.FileOutputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

/**
 * The only writer of `keepalive.json`: whether the user lets Shizuku wake the Runtime after the
 * system ends it (on unless turned off), and which system exemptions DroidBridge itself granted
 * for that, so turning keep-alive off withdraws exactly those and never one the user granted.
 */
internal class KeepAliveSettings(directory: File) {
    data class State(
        val enabled: Boolean = true,
        val idleExemptionGranted: Boolean = false,
        val backgroundGranted: Boolean = false,
    )

    private val file = File(directory, FILE_NAME)
    private val lock = Any()
    private var committed: State? = null

    fun state(): Result<State> = synchronized(lock) { load() }

    fun update(change: (State) -> State): Result<State> = synchronized(lock) {
        load().mapCatching { current ->
            val next = change(current)
            if (next != current) commit(next)
            next
        }
    }

    private fun load(): Result<State> {
        committed?.let { return Result.success(it) }
        if (!file.exists()) return Result.success(State().also { committed = it })
        return runCatching { decode(file.readText()) }.onSuccess { committed = it }
    }

    private fun commit(next: State) {
        val directory = checkNotNull(file.parentFile)
        check(directory.isDirectory || directory.mkdirs())
        val temporary = File(directory, "$FILE_NAME.tmp")
        try {
            FileOutputStream(temporary).use { output ->
                output.write(encode(next).encodeToByteArray())
                output.fd.sync()
            }
            Files.move(
                temporary.toPath(),
                file.toPath(),
                StandardCopyOption.ATOMIC_MOVE,
                StandardCopyOption.REPLACE_EXISTING,
            )
            val descriptor = Os.open(directory.absolutePath, OsConstants.O_RDONLY, 0)
            try {
                Os.fsync(descriptor)
            } finally {
                Os.close(descriptor)
            }
            committed = next
        } catch (failure: Exception) {
            // The file is authoritative again on the next read; the snapshot never runs ahead of it.
            committed = null
            throw failure
        } finally {
            if (temporary.exists()) temporary.delete()
        }
    }

    private companion object {
        const val FILE_NAME = "keepalive.json"
        const val SCHEMA_VERSION = 1
        val KEYS = setOf("schema_version", "enabled", "idle_exemption_granted", "background_granted")

        fun encode(state: State): String = buildJsonObject {
            put("schema_version", SCHEMA_VERSION)
            put("enabled", state.enabled)
            put("idle_exemption_granted", state.idleExemptionGranted)
            put("background_granted", state.backgroundGranted)
        }.toString()

        fun decode(text: String): State {
            val value = Json.parseToJsonElement(text).jsonObject
            require(value.keys == KEYS)
            require(value.getValue("schema_version").jsonPrimitive.content == SCHEMA_VERSION.toString())
            return State(
                enabled = value.getValue("enabled").jsonPrimitive.boolean,
                idleExemptionGranted = value.getValue("idle_exemption_granted").jsonPrimitive.boolean,
                backgroundGranted = value.getValue("background_granted").jsonPrimitive.boolean,
            )
        }
    }
}
