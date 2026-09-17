package com.droidbridge.android

import com.droidbridge.android.execution.android.AndroidExecutionBridge
import com.droidbridge.android.execution.android.AndroidExecutionRegistry
import com.droidbridge.android.execution.android.AndroidExecutionResult
import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.CapabilityRegistration
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import com.droidbridge.android.runtimehost.DaemonHandshake
import com.droidbridge.android.runtimehost.DaemonErrorToken
import com.droidbridge.android.runtimehost.DaemonHostToken
import com.droidbridge.android.runtimehost.DaemonMessageHistory
import com.droidbridge.android.runtimehost.DaemonMessageKind
import com.droidbridge.android.runtimehost.DaemonOperationToken
import com.droidbridge.android.runtimehost.DaemonOwnerFence
import com.droidbridge.android.runtimehost.DaemonProtocol
import com.droidbridge.android.runtimehost.DaemonRoleToken
import com.droidbridge.android.runtimehost.DaemonWireCodec
import com.droidbridge.android.runtimehost.DaemonWireEnvelope
import com.droidbridge.android.runtimehost.CompanionCapabilityFacts
import com.droidbridge.android.runtimehost.HostPromotionState
import com.droidbridge.android.runtimehost.MagiskHostStatusAction
import com.droidbridge.android.runtimehost.RuntimeSessionState
import com.droidbridge.android.runtimehost.companionFailurePayload
import com.droidbridge.android.runtimehost.companionResponseInstance
import com.droidbridge.android.runtimehost.companionResultPayload
import com.droidbridge.android.runtimehost.companionResultRoles
import com.droidbridge.android.runtimehost.controlAcknowledged
import com.droidbridge.android.runtimehost.decodeCompanionExecution
import com.droidbridge.android.runtimehost.shouldAttemptRuntimeStart
import com.droidbridge.android.runtimehost.shouldRecoverUncommittedApkTransition
import com.droidbridge.android.runtimehost.responseFenceMatches
import com.droidbridge.android.runtimehost.decideMagiskHostStatus
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class I7_GatesTest {
    @Test
    fun i7_g05_companionSnapshotIsGenerationFencedAndClosedToAppOwnedKeys() {
        val facts = CompanionCapabilityFacts()

        assertTrue(facts.register("shizuku.shell", "unknown", "CONNECTING", 3, false))
        assertFalse(facts.register("shizuku.shell", "unavailable", "BINDER_UNAVAILABLE", 2, false))
        assertEquals(
            listOf("shizuku.shell" to 3L),
            facts.snapshot().map { it.key to it.sourceGeneration },
        )
        assertTrue(
            runCatching {
                facts.register("magisk.root", "available", "", 4, true)
            }.isFailure,
        )
    }

    @Test
    fun i7_g01_socketIdentityAndRootHandshakeAreExact() {
        assertEquals(
            "droidbridge.com.droidbridge.android.u0.v1",
            DaemonProtocol.socketName("com.droidbridge.android"),
        )
        assertEquals(
            "droidbridge.com.droidbridge.android.debug.u0.v1",
            DaemonProtocol.socketName("com.droidbridge.android.debug"),
        )
        val owner = DaemonOwnerFence(EPOCH, DaemonHostToken.ApkRuntime, 3, INSTANCE)
        val handshake = DaemonHandshake(
            1,
            DaemonRoleToken.Droidbridged,
            "com.droidbridge.android",
            0,
            EPOCH,
            DaemonHostToken.ApkRuntime,
            3,
            null,
        )
        assertTrue(handshake.accepts(0, "com.droidbridge.android", owner))
        assertFalse(handshake.accepts(2000, "com.droidbridge.android", owner))
        assertFalse(handshake.copy(hostGeneration = 4).accepts(0, "com.droidbridge.android", owner))
    }

    @Test
    fun i7_g10_framingRejectsUnknownFieldsAndOversizeBodies() {
        val handshake = DaemonHandshake(
            1,
            DaemonRoleToken.Droidbridged,
            "com.droidbridge.android",
            0,
            EPOCH,
            DaemonHostToken.ApkRuntime,
            3,
            null,
        )
        val framed = DaemonProtocol.frame(DaemonProtocol.encodeHandshake(handshake))
        assertEquals(handshake, DaemonProtocol.decodeHandshake(DaemonProtocol.body(framed)))
        assertTrue(
            runCatching {
                DaemonProtocol.decodeHandshake(
                    DaemonProtocol.encodeHandshake(handshake).let { encoded ->
                        encoded.copyOf(encoded.size - 1) + ",\"extra\":true}".encodeToByteArray()
                    },
                )
            }.isFailure,
        )
        assertTrue(runCatching { DaemonProtocol.frame(ByteArray(262_145)) }.isFailure)
    }

    @Test
    fun i7_g10_protocolCatalogRoundTripsAndRejectsUnknownTokens() {
        DaemonMessageKind.entries.forEach { token ->
            assertEquals(token, DaemonProtocol.decodeMessageKind(token.wire))
        }
        DaemonOperationToken.entries.forEach { token ->
            assertEquals(token, DaemonProtocol.decodeOperation(token.wire))
        }
        DaemonHostToken.entries.forEach { token ->
            assertEquals(token, DaemonProtocol.decodeHost(token.wire))
        }
        DaemonErrorToken.entries.forEach { token ->
            assertEquals(token, DaemonProtocol.decodeError(token.wire))
        }
        assertEquals(
            DaemonOperationToken.entries.size,
            DaemonOperationToken.entries.map { it.wire }.toSet().size,
        )
        assertEquals(
            DaemonErrorToken.entries.size,
            DaemonErrorToken.entries.map { it.wire }.toSet().size,
        )
        assertTrue(runCatching { DaemonProtocol.decodeMessageKind("unknown") }.isFailure)
        assertTrue(runCatching { DaemonProtocol.decodeOperation("unknown") }.isFailure)
        assertTrue(runCatching { DaemonProtocol.decodeHost("unknown") }.isFailure)
        assertTrue(runCatching { DaemonProtocol.decodeError("unknown") }.isFailure)
    }

    @Test
    fun i7_g10_descriptorRolesMustMatchReceivedCountAndKnownOrder() {
        val envelope = DaemonWireEnvelope(
            kind = DaemonMessageKind.Request,
            messageId = INSTANCE,
            replyTo = null,
            runtimeEpoch = EPOCH,
            hostGeneration = 3,
            runtimeInstanceId = INSTANCE,
            operation = DaemonOperationToken.RuntimeForward,
            payload = buildJsonObject { put("protocol_version", 1) },
            fdRoles = listOf("stdin", "stdout"),
        )
        val encoded = DaemonWireCodec.encode(envelope)
        assertEquals(envelope, DaemonWireCodec.decode(encoded, 2))
        assertTrue(runCatching { DaemonWireCodec.decode(encoded, 1) }.isFailure)
        assertTrue(
            runCatching {
                DaemonWireCodec.encode(envelope.copy(fdRoles = listOf("unknown")))
            }.isFailure,
        )
    }

    @Test
    fun i7_g10_responseOwnerFenceMustMatchThePendingRequest() {
        val owner = DaemonOwnerFence(EPOCH, DaemonHostToken.MagiskBackend, 3, INSTANCE)
        val response = DaemonWireEnvelope(
            kind = DaemonMessageKind.Response,
            messageId = "00000000-0000-4000-8000-000000000003",
            replyTo = INSTANCE,
            runtimeEpoch = EPOCH,
            hostGeneration = 3,
            runtimeInstanceId = INSTANCE,
            operation = DaemonOperationToken.RuntimeForward,
            payload = buildJsonObject {},
            fdRoles = emptyList(),
        )

        assertTrue(responseFenceMatches(owner, response, requireInstance = true))
        assertFalse(responseFenceMatches(owner, response.copy(hostGeneration = 4), requireInstance = true))
        assertFalse(responseFenceMatches(owner.copy(runtimeInstanceId = null), response, requireInstance = true))
    }

    @Test
    fun i7_g07_deferredPromotionRetriesOnlyAfterAnotherIdleHint() {
        val state = HostPromotionState()
        assertFalse(state.tryBegin())
        state.observeBackendReady()
        assertTrue(state.tryBegin())
        assertFalse(state.tryBegin())
        assertFalse(state.finishAttempt())
        assertFalse(state.tryBegin())
        state.observeIdleHint()
        assertTrue(state.tryBegin())
        state.observeIdleHint()
        assertTrue(state.finishAttempt())
        assertTrue(state.tryBegin())
        assertFalse(state.finishAttempt())
        state.clearBackend()
        assertFalse(state.tryBegin())
    }

    @Test
    fun i7_g02_pendingSourceIntentRecoversAfterOriginalDemotionReasonClears() {
        assertEquals(
            MagiskHostStatusAction.RecoverDemotion,
            decideMagiskHostStatus(
                ready = true,
                cleanupReady = true,
                requiresApkHost = false,
                transitionPresent = true,
                pendingSourceTransition = true,
            ),
        )
        assertEquals(
            MagiskHostStatusAction.EstablishCurrentHost,
            decideMagiskHostStatus(
                ready = true,
                cleanupReady = true,
                requiresApkHost = false,
                transitionPresent = false,
                pendingSourceTransition = false,
            ),
        )
        assertEquals(
            MagiskHostStatusAction.EstablishCurrentHost,
            decideMagiskHostStatus(
                ready = true,
                cleanupReady = true,
                requiresApkHost = true,
                transitionPresent = true,
                pendingSourceTransition = false,
            ),
        )
        assertEquals(
            MagiskHostStatusAction.Wait,
            decideMagiskHostStatus(
                ready = false,
                cleanupReady = false,
                requiresApkHost = true,
                transitionPresent = true,
                pendingSourceTransition = true,
            ),
        )
        assertEquals(
            MagiskHostStatusAction.BeginDemotion,
            decideMagiskHostStatus(
                ready = false,
                cleanupReady = true,
                requiresApkHost = false,
                transitionPresent = false,
                pendingSourceTransition = false,
            ),
        )
    }

    @Test
    fun i7_g02_runtimeStartRetryCannotOverwriteAnOwnedTransitionProjection() {
        assertFalse(
            shouldAttemptRuntimeStart(
                RuntimeSessionState(
                    host = DaemonHostToken.MagiskBackend,
                    startFailure = DaemonErrorToken.HostTransitionPending.wire,
                ),
            ),
        )
        assertTrue(shouldAttemptRuntimeStart(RuntimeSessionState()))
    }

    @Test
    fun i7_g02_transitionControlRequiresExactPositiveAcknowledgement() {
        assertTrue(controlAcknowledged(buildJsonObject { put("released", true) }, "released"))
        assertFalse(controlAcknowledged(buildJsonObject { put("released", false) }, "released"))
        assertFalse(
            controlAcknowledged(
                buildJsonObject {
                    put("released", true)
                    put(
                        "error",
                        buildJsonObject { put("code", DaemonErrorToken.StaleAuthority.wire) },
                    )
                },
                "released",
            ),
        )
        assertFalse(controlAcknowledged(buildJsonObject { put("aborted", true) }, "released"))
    }

    @Test
    fun i7_g02_deadApkSourceRecoversOnlyItsUncommittedTransition() {
        assertTrue(
            shouldRecoverUncommittedApkTransition(
                startCode = DaemonErrorToken.HostTransitionPending.wire,
                ownerHost = DaemonHostToken.ApkRuntime,
                transitionState = "source_pending",
            ),
        )
        assertFalse(
            shouldRecoverUncommittedApkTransition(
                startCode = DaemonErrorToken.HostTransitionPending.wire,
                ownerHost = DaemonHostToken.MagiskBackend,
                transitionState = "source_pending",
            ),
        )
        assertFalse(
            shouldRecoverUncommittedApkTransition(
                startCode = DaemonErrorToken.HostTransitionPending.wire,
                ownerHost = DaemonHostToken.ApkRuntime,
                transitionState = "target_committed",
            ),
        )
        assertFalse(
            shouldRecoverUncommittedApkTransition(
                startCode = DaemonErrorToken.IoError.wire,
                ownerHost = DaemonHostToken.ApkRuntime,
                transitionState = "source_pending",
            ),
        )
    }

    @Test
    fun i7_g10_messageHistoryRenewsOnlyWithAFreshConnection() {
        val exhausted = DaemonMessageHistory()
        repeat(DaemonMessageHistory.MAX_MESSAGES_PER_DIRECTION) { value ->
            exhausted.recordIncoming(sequenceId(0x10000000, value))
            exhausted.recordOutgoing(sequenceId(0x20000000, value))
        }
        assertTrue(
            runCatching {
                exhausted.recordIncoming(
                    sequenceId(0x10000000, DaemonMessageHistory.MAX_MESSAGES_PER_DIRECTION),
                )
            }.isFailure,
        )
        assertTrue(
            runCatching {
                exhausted.recordOutgoing(
                    sequenceId(0x20000000, DaemonMessageHistory.MAX_MESSAGES_PER_DIRECTION),
                )
            }.isFailure,
        )

        DaemonMessageHistory().recordIncoming(sequenceId(0x10000000, 0))
    }

    @Test
    fun i7_g10_companionExecutionDecodesExactlyOneAdmittedPrimitive() {
        val executionId = sequenceId(0x40000000, 1)
        val body = Json.parseToJsonElement(
            """
            {
                "primitive": "ContentInspect",
                "payload": {"target": {"type": "content_uri", "value": "content://media/a"}},
                "execution_id": "$executionId"
            }
            """.trimIndent(),
        )

        val execution = requireNotNull(decodeCompanionExecution(body))
        assertEquals(AndroidPrimitive.ContentInspect, execution.primitive)
        assertEquals(executionId, execution.executionId)
        assertEquals(
            body.jsonObject.getValue("payload"),
            Json.parseToJsonElement(execution.payload.decodeToString()),
        )

        fun decode(fragment: String) = decodeCompanionExecution(Json.parseToJsonElement(fragment))
        assertNull(decode("""{"primitive":"ContentInspect","payload":{},"execution_id":"$executionId","extra":1}"""))
        assertNull(decode("""{"primitive":"ContentInspect","execution_id":"$executionId"}"""))
        assertNull(decode("""{"primitive":"UnknownPrimitive","payload":{},"execution_id":"$executionId"}"""))
        assertNull(decode("""{"primitive":7,"payload":{},"execution_id":"$executionId"}"""))
        assertNull(decode("""{"primitive":"ContentInspect","payload":{},"execution_id":"not-a-uuid"}"""))
        assertNull(decodeCompanionExecution(Json.parseToJsonElement("[]")))
    }

    @Test
    fun i7_g10_companionExecutionResolvesItsExecutorByPrimitiveAndGeneration() {
        val framework = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf()) }
        val replacement = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf()) }
        val registry = AndroidExecutionRegistry { _, _, _, _, _ -> true }
        assertTrue(
            registry.register(
                CapabilityRegistration(
                    key = "android.framework",
                    state = RegisteredCapabilityState.Available,
                    reason = null,
                    sourceGeneration = 7,
                    executor = framework,
                    primitives = setOf(
                        AndroidPrimitive.ContentInspect,
                        AndroidPrimitive.ContentOpenRead,
                    ),
                ),
            ),
        )
        assertSame(framework, registry.executor(AndroidPrimitive.ContentInspect, 7))
        assertSame(framework, registry.executor(AndroidPrimitive.ContentOpenRead, 7))
        assertSame(framework, registry.executor("android.framework", 7))
        assertNull(registry.executor(AndroidPrimitive.ContentInspect, 6))
        assertNull(registry.executor(AndroidPrimitive.PackageInspect, 7))

        assertTrue(
            registry.register(
                CapabilityRegistration(
                    key = "android.framework",
                    state = RegisteredCapabilityState.Available,
                    reason = null,
                    sourceGeneration = 8,
                    executor = replacement,
                    primitives = setOf(AndroidPrimitive.ContentInspect),
                ),
            ),
        )
        assertSame(replacement, registry.executor(AndroidPrimitive.ContentInspect, 8))
        assertSame(replacement, registry.executor("android.framework", 8))
        assertNull(registry.executor(AndroidPrimitive.ContentOpenRead, 8))
        assertTrue(
            runCatching {
                registry.register(
                    CapabilityRegistration(
                        key = "android.framework",
                        state = RegisteredCapabilityState.Unavailable,
                        reason = "BINDER_UNAVAILABLE",
                        sourceGeneration = 9,
                        primitives = setOf(AndroidPrimitive.ContentInspect),
                    ),
                )
            }.isFailure,
        )
    }

    @Test
    fun i7_g10_companionReplyRepeatsOnlyTheLiveInstanceAndStaysWireDecodable() {
        val owner = DaemonOwnerFence(EPOCH, DaemonHostToken.MagiskBackend, 3, INSTANCE)
        val execute = companionRequest(DaemonOperationToken.CompanionExecute, INSTANCE)
        assertEquals(INSTANCE, companionResponseInstance(execute, owner))
        assertTrue(
            runCatching {
                companionResponseInstance(execute, owner.copy(runtimeInstanceId = null))
            }.isFailure,
        )
        assertTrue(
            runCatching {
                companionResponseInstance(
                    execute,
                    owner.copy(runtimeInstanceId = sequenceId(0x50000000, 1)),
                )
            }.isFailure,
        )
        val snapshot = companionRequest(DaemonOperationToken.CapabilitySnapshot, null)
        assertEquals(INSTANCE, companionResponseInstance(snapshot, owner))
        assertNull(companionResponseInstance(snapshot, owner.copy(runtimeInstanceId = null)))

        val reply = DaemonWireEnvelope(
            kind = DaemonMessageKind.Response,
            messageId = sequenceId(0x60000000, 1),
            replyTo = execute.messageId,
            runtimeEpoch = EPOCH,
            hostGeneration = 3,
            runtimeInstanceId = companionResponseInstance(execute, owner),
            operation = execute.operation,
            payload = companionFailurePayload(DaemonErrorToken.CapabilityUnavailable.wire),
            fdRoles = emptyList(),
        )
        assertEquals(INSTANCE, DaemonWireCodec.decode(DaemonWireCodec.encode(reply), 0).runtimeInstanceId)
    }

    @Test
    fun i7_g10_companionResultsCarryOnlyWireDescriptorRolesAndTypedFailures() {
        assertEquals(
            Json.parseToJsonElement("""{"payload":{"total_size":12}}"""),
            companionResultPayload("""{"total_size":12}""".encodeToByteArray()),
        )
        assertNull(companionResultPayload("not json".encodeToByteArray()))
        assertEquals(
            Json.parseToJsonElement("""{"error":{"code":"STALE_AUTHORITY","retryable":false}}"""),
            companionFailurePayload("STALE_AUTHORITY"),
        )
        assertEquals(
            Json.parseToJsonElement("""{"error":{"code":"INTERNAL_ERROR","retryable":false}}"""),
            companionFailurePayload("NOT_A_CODE"),
        )
        assertEquals(listOf("stdout", "content"), companionResultRoles(listOf("stdout", "content")))
        assertNull(companionResultRoles(listOf("content_read")))
        assertEquals(emptyList<String>(), companionResultRoles(emptyList()))
        assertTrue(DaemonProtocol.isCanonicalDescriptorRole("content"))
        assertFalse(DaemonProtocol.isCanonicalDescriptorRole("content_read"))
    }

    private fun companionRequest(
        operation: DaemonOperationToken,
        runtimeInstanceId: String?,
    ): DaemonWireEnvelope = DaemonWireEnvelope(
        kind = DaemonMessageKind.Request,
        messageId = sequenceId(0x70000000, 1),
        replyTo = null,
        runtimeEpoch = EPOCH,
        hostGeneration = 3,
        runtimeInstanceId = runtimeInstanceId,
        operation = operation,
        payload = buildJsonObject {},
        fdRoles = emptyList(),
    )

    private fun sequenceId(prefix: Int, value: Int): String =
        "%08x-0000-4000-8000-%012x".format(prefix, value)

    companion object {
        private const val EPOCH = "00000000-0000-4000-8000-000000000001"
        private const val INSTANCE = "00000000-0000-4000-8000-000000000002"
    }
}
