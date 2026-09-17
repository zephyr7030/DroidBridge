package com.droidbridge.android.runtimehost

import java.nio.ByteBuffer
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put

internal enum class DaemonMessageKind(val wire: String) {
    Request("request"),
    Response("response"),
    Cancel("cancel"),
}

@Serializable
internal enum class DaemonRoleToken(val wire: String) {
    @SerialName("droidbridged")
    Droidbridged("droidbridged"),

    @SerialName("apk_runtime")
    ApkRuntime("apk_runtime"),
}

internal enum class DaemonOperationToken(
    val wire: String,
    val control: Boolean = false,
    val instanceFenced: Boolean = false,
) {
    HostStatus("HostStatus", control = true),
    HostPrepareTransition("HostPrepareTransition", control = true),
    HostAbortTransition("HostAbortTransition", control = true),
    HostRelease("HostRelease", control = true),
    HostActivate("HostActivate", control = true),
    RuntimeForward("RuntimeForward", instanceFenced = true),
    RuntimeCancel("RuntimeCancel", control = true, instanceFenced = true),
    NetworkDefaultChanged("NetworkDefaultChanged", instanceFenced = true),
    NetworkAttachment("NetworkAttachment", instanceFenced = true),
    CompanionExecute("CompanionExecute", instanceFenced = true),
    CompanionCancel("CompanionCancel", control = true, instanceFenced = true),
    CapabilitySnapshot("CapabilitySnapshot"),
    DiagnosticsSnapshot("DiagnosticsSnapshot"),
    MaintenanceStatus("MaintenanceStatus", control = true),
    MaintenanceInstallApk("MaintenanceInstallApk"),
    MaintenanceInstallModule("MaintenanceInstallModule"),
}

@Serializable
internal enum class DaemonHostToken(val wire: String) {
    @SerialName("apk_runtime")
    ApkRuntime("apk_runtime"),

    @SerialName("magisk_backend")
    MagiskBackend("magisk_backend"),
}

internal enum class DaemonErrorToken(val wire: String) {
    InvalidArgument("INVALID_ARGUMENT"),
    NotFound("NOT_FOUND"),
    AlreadyExists("ALREADY_EXISTS"),
    PermissionDenied("PERMISSION_DENIED"),
    CapabilityUnavailable("CAPABILITY_UNAVAILABLE"),
    Unsupported("UNSUPPORTED"),
    StaleAuthority("STALE_AUTHORITY"),
    StaleReference("STALE_REFERENCE"),
    RevisionConflict("REVISION_CONFLICT"),
    Timeout("TIMEOUT"),
    Cancelled("CANCELLED"),
    IoError("IO_ERROR"),
    ProtocolIncompatible("PROTOCOL_INCOMPATIBLE"),
    ResourceLimit("RESOURCE_LIMIT"),
    InternalError("INTERNAL_ERROR"),
    NotEmpty("NOT_EMPTY"),
    ArchiveCorrupt("ARCHIVE_CORRUPT"),
    ArchiveEncrypted("ARCHIVE_ENCRYPTED"),
    RunAsUnavailable("RUN_AS_UNAVAILABLE"),
    ExecutionFailed("EXECUTION_FAILED"),
    CancelFailed("CANCEL_FAILED"),
    CaptureFailed("CAPTURE_FAILED"),
    HostTransitionPending("HOST_TRANSITION_PENDING"),
}

internal data class DaemonOwnerFence(
    val runtimeEpoch: String,
    val host: DaemonHostToken,
    val hostGeneration: Long,
    val runtimeInstanceId: String?,
)

@Serializable
internal data class DaemonHandshake(
    @SerialName("protocol_version") val protocolVersion: Int,
    val role: DaemonRoleToken,
    @SerialName("package") val packageName: String,
    @SerialName("user_id") val userId: Int,
    @SerialName("runtime_epoch") val runtimeEpoch: String,
    val host: DaemonHostToken,
    @SerialName("host_generation") val hostGeneration: Long,
    @SerialName("runtime_instance_id") val runtimeInstanceId: String?,
) {
    fun accepts(peerUid: Int, expectedPackage: String, owner: DaemonOwnerFence): Boolean =
        peerUid == 0 &&
            protocolVersion == DaemonProtocol.PROTOCOL_VERSION &&
            role == DaemonRoleToken.Droidbridged &&
            packageName == expectedPackage &&
            userId == 0 &&
            runtimeEpoch == owner.runtimeEpoch &&
            host == owner.host &&
            hostGeneration == owner.hostGeneration &&
            DaemonProtocol.isUuid(runtimeEpoch) &&
            (runtimeInstanceId == null || DaemonProtocol.isUuid(runtimeInstanceId))
}

internal object DaemonProtocol {
    const val PROTOCOL_VERSION = 1
    const val MAX_FRAME_BYTES = 262_144
    private val uuid = Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")
    private val json = Json {
        ignoreUnknownKeys = false
        explicitNulls = true
        encodeDefaults = true
    }

    fun socketName(packageName: String): String {
        require(packageName == "com.droidbridge.android" || packageName == "com.droidbridge.android.debug")
        return "droidbridge.$packageName.u0.v1"
    }

    fun encodeHandshake(handshake: DaemonHandshake): ByteArray =
        json.encodeToString(handshake).encodeToByteArray()

    fun decodeHandshake(body: ByteArray): DaemonHandshake {
        require(body.isNotEmpty() && body.size <= MAX_FRAME_BYTES)
        return json.decodeFromString<DaemonHandshake>(body.decodeToString())
    }

    fun frame(body: ByteArray): ByteArray {
        require(body.isNotEmpty() && body.size <= MAX_FRAME_BYTES)
        return ByteBuffer.allocate(4 + body.size)
            .putInt(body.size)
            .put(body)
            .array()
    }

    fun body(frame: ByteArray): ByteArray {
        require(frame.size >= 5)
        val length = ByteBuffer.wrap(frame, 0, 4).int
        require(length in 1..MAX_FRAME_BYTES && frame.size == length + 4)
        return frame.copyOfRange(4, frame.size)
    }

    fun isUuid(value: String): Boolean = uuid.matches(value)

    fun decodeMessageKind(wire: String): DaemonMessageKind =
        requireNotNull(DaemonMessageKind.entries.firstOrNull { it.wire == wire }) {
            "unknown daemon message kind"
        }

    fun decodeOperation(wire: String): DaemonOperationToken =
        requireNotNull(DaemonOperationToken.entries.firstOrNull { it.wire == wire }) {
            "unknown daemon operation"
        }

    fun decodeHost(wire: String): DaemonHostToken =
        requireNotNull(DaemonHostToken.entries.firstOrNull { it.wire == wire }) {
            "unknown daemon host"
        }

    fun decodeError(wire: String): DaemonErrorToken =
        requireNotNull(DaemonErrorToken.entries.firstOrNull { it.wire == wire }) {
            "unknown daemon error"
        }

    fun isCanonicalDescriptorRole(role: String): Boolean = role in DaemonWireCodec.fdRoles
}

internal object DaemonWireCodec {
    private val json = Json { ignoreUnknownKeys = false }
    val fdRoles = setOf(
        "execution_guard_proof",
        "verified_apk",
        "verified_module_zip",
        "visual_raw_frame",
        "visual_source_image",
        "visual_encoded_image",
        "stdin",
        "stdout",
        "stderr",
        "content",
        "mcp_artifact",
    )

    fun encode(envelope: DaemonWireEnvelope): ByteArray {
        validate(envelope, envelope.fdRoles.size)
        val value = buildJsonObject {
            put("protocol_version", DaemonProtocol.PROTOCOL_VERSION)
            put("kind", envelope.kind.wire)
            put("message_id", envelope.messageId)
            envelope.replyTo?.let { put("reply_to", it) }
            put("runtime_epoch", envelope.runtimeEpoch)
            put("host_generation", envelope.hostGeneration)
            put("runtime_instance_id", envelope.runtimeInstanceId?.let(::JsonPrimitive) ?: JsonNull)
            put("operation", envelope.operation.wire)
            put("payload", envelope.payload)
            put("fd_roles", JsonArray(envelope.fdRoles.map(::JsonPrimitive)))
        }
        return json.encodeToString(JsonObject.serializer(), value).encodeToByteArray()
    }

    fun decode(body: ByteArray, descriptorCount: Int): DaemonWireEnvelope {
        require(body.isNotEmpty() && body.size <= DaemonProtocol.MAX_FRAME_BYTES)
        val value = json.parseToJsonElement(body.decodeToString()).jsonObject
        val kind = DaemonProtocol.decodeMessageKind(value.string("kind"))
        val required = mutableSetOf(
            "protocol_version",
            "kind",
            "message_id",
            "runtime_epoch",
            "host_generation",
            "runtime_instance_id",
            "operation",
            "payload",
            "fd_roles",
        )
        if (kind != DaemonMessageKind.Request) required += "reply_to"
        require(value.keys == required)
        require(value.long("protocol_version") == 1L)
        val envelope = DaemonWireEnvelope(
            kind = kind,
            messageId = value.string("message_id"),
            replyTo = value["reply_to"]?.jsonPrimitive?.contentOrNull,
            runtimeEpoch = value.string("runtime_epoch"),
            hostGeneration = value.long("host_generation"),
            runtimeInstanceId = value["runtime_instance_id"]?.jsonPrimitive?.contentOrNull,
            operation = DaemonProtocol.decodeOperation(value.string("operation")),
            payload = requireNotNull(value["payload"]),
            fdRoles = value.getValue("fd_roles").jsonArray.map { it.jsonPrimitive.content },
        )
        validate(envelope, descriptorCount)
        return envelope
    }

    private fun validate(envelope: DaemonWireEnvelope, descriptorCount: Int) {
        require(DaemonProtocol.isUuid(envelope.messageId))
        require(DaemonProtocol.isUuid(envelope.runtimeEpoch))
        require(envelope.hostGeneration > 0)
        require(envelope.fdRoles.size <= 4 && envelope.fdRoles.size == descriptorCount)
        require(envelope.fdRoles.all(fdRoles::contains))
        require((envelope.kind == DaemonMessageKind.Request) == (envelope.replyTo == null))
        envelope.replyTo?.let { require(DaemonProtocol.isUuid(it)) }
        envelope.runtimeInstanceId?.let { require(DaemonProtocol.isUuid(it)) }
        if (envelope.operation.instanceFenced) {
            require(envelope.runtimeInstanceId != null)
        }
    }

    private fun JsonObject.string(name: String): String = getValue(name).jsonPrimitive.content

    private fun JsonObject.long(name: String): Long =
        getValue(name).jsonPrimitive.longOrNull ?: throw IllegalArgumentException(name)
}
