package com.droidbridge.android

import com.droidbridge.android.client.AvailabilityFact
import com.droidbridge.android.client.AvailabilityState
import com.droidbridge.android.client.CapabilityAction
import com.droidbridge.android.client.CapabilityRowKey
import com.droidbridge.android.client.CapabilityRowState
import com.droidbridge.android.client.CapabilityRows
import com.droidbridge.android.client.RefreshCoordinator
import com.droidbridge.android.client.RuntimeReadiness
import com.droidbridge.android.client.RuntimeSnapshot
import com.droidbridge.android.client.ClientState
import com.droidbridge.android.client.resolveContextRefresh
import com.droidbridge.android.execution.android.AndroidExecutionBridge
import com.droidbridge.android.execution.android.AndroidExecutionRegistry
import com.droidbridge.android.execution.android.AndroidExecutionResult
import com.droidbridge.android.execution.android.CapabilityRegistration
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import com.droidbridge.android.runtimehost.NativeRuntime
import com.droidbridge.android.runtimehost.DaemonErrorToken
import com.droidbridge.android.runtimehost.DaemonHostToken
import com.droidbridge.android.runtimehost.RuntimeFence
import com.droidbridge.android.runtimehost.RuntimeHostController
import com.droidbridge.android.runtimehost.RuntimeSessionState
import com.droidbridge.android.runtimehost.decideMagiskHostStatus
import com.droidbridge.android.runtimehost.MagiskHostStatusAction
import com.droidbridge.android.runtimehost.shouldRecoverUncommittedApkTransition
import com.droidbridge.android.runtimehost.shouldResumeCommittedApkTransition
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class I5_GatesTest {
    @Test
    fun I5_G01_apkRuntimeProjectionRetainsAuthoritativeHostIdentity() {
        val snapshot = snapshot(readiness = RuntimeReadiness.Ready, hostGeneration = 7)

        assertEquals("apk_runtime", snapshot.host)
        assertEquals(7, snapshot.hostGeneration)
        assertEquals(RuntimeReadiness.Ready, snapshot.readiness)
    }

    @Test
    fun I5_G02_binderHintsRequireAnAuthoritativeRefresh() {
        val coordinator = RefreshCoordinator()

        assertEquals(0L, coordinator.hint(1_000))
        coordinator.started(1_000)
        assertNull(coordinator.hint(1_001))
        assertEquals(99L, coordinator.finished(1_001))
    }

    @Test
    fun I5_G03_androidRegistrationRejectsStaleGenerationAndInvalidExecutorState() {
        val calls = mutableListOf<List<Any>>()
        val registry = AndroidExecutionRegistry { key, state, reason, generation, hasExecutor ->
            calls += listOf(key, state, reason, generation, hasExecutor)
            true
        }
        val executor = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf()) }

        assertTrue(registry.register(registration("visual.accessibility", 2, executor)))
        assertFalse(registry.register(registration("visual.accessibility", 1, executor)))
        assertSame(executor, registry.executor("visual.accessibility", 2))
        assertNull(registry.executor("visual.accessibility", 1))
        assertEquals(1, calls.size)
        val rejected = runCatching {
            registry.register(
                CapabilityRegistration(
                    "visual.accessibility",
                    RegisteredCapabilityState.Unavailable,
                    "DISCONNECTED",
                    3,
                    executor,
                ),
            )
        }
        assertTrue(rejected.isFailure)
    }

    @Test
    fun I5_G04_uiProjectsCapabilityTruthWithoutOwningIt() {
        val rows = CapabilityRows.project(
            snapshot(
                grants = allUnavailable(),
                capabilities = allCapabilitiesUnavailable(),
            ),
        )

        assertEquals(
            listOf(
                CapabilityRowKey.Runtime,
                CapabilityRowKey.RootBackend,
                CapabilityRowKey.Shizuku,
                CapabilityRowKey.LocalNetwork,
                CapabilityRowKey.NotificationAccess,
                CapabilityRowKey.ExactAlarm,
                CapabilityRowKey.Accessibility,
                CapabilityRowKey.ScreenCapture,
            ),
            rows.map { it.key },
        )
        assertEquals(CapabilityAction.StartCapture, rows.last().action)
    }

    @Test
    fun I5_G04_defaultAndRuntimeProcessesHaveDisjointGraphRoles() {
        val packageName = "com.droidbridge.android"

        assertEquals(ProcessRole.Default, classifyProcess(packageName, packageName))
        assertEquals(ProcessRole.Runtime, classifyProcess("$packageName:runtime", packageName))
        assertEquals(ProcessRole.Unexpected, classifyProcess("$packageName:other", packageName))
    }

    @Test
    fun I5_G03_downstreamAdaptersRegisterThroughOneGenerationFencedSurface() {
        val accepted = mutableListOf<String>()
        val registry = AndroidExecutionRegistry { key, _, _, _, _ -> accepted += key; true }
        val shizuku = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf(1)) }
        val magiskFramework = AndroidExecutionBridge { AndroidExecutionResult(byteArrayOf(2)) }

        assertTrue(registry.register(registration("shizuku.shell", 11, shizuku)))
        assertTrue(registry.register(registration("magisk.framework", 12, magiskFramework)))
        assertSame(shizuku, registry.executor("shizuku.shell", 11))
        assertSame(magiskFramework, registry.executor("magisk.framework", 12))
        assertEquals(listOf("shizuku.shell", "magisk.framework"), accepted)
    }

    @Test
    fun I5_G09_refreshStartsAreRateLimitedAndDirtyHintsCoalesce() {
        val coordinator = RefreshCoordinator(100)

        assertEquals(0L, coordinator.hint(0))
        coordinator.started(0)
        repeat(20) { assertNull(coordinator.hint(it.toLong() + 1)) }
        assertEquals(79L, coordinator.finished(21))
        coordinator.started(100)
        assertNull(coordinator.finished(101))
        assertEquals(0L, coordinator.hint(200))
    }

    @Test
    fun I5_G08_cleanupQuarantineHasDiagnosticsAndNeverRetry() {
        val rows = CapabilityRows.project(
            snapshot(
                readiness = RuntimeReadiness.Unavailable,
                runtimeReason = "CLEANUP_UNVERIFIED",
            ),
        )
        val runtime = rows.first { it.key == CapabilityRowKey.Runtime }

        assertEquals(CapabilityRowState.Unavailable, runtime.state)
        assertEquals(CapabilityAction.Diagnostics, runtime.action)
        assertFalse(rows.any { it.key == CapabilityRowKey.Runtime && it.action == CapabilityAction.Retry })
    }

    @Test
    fun I5_G09_failedRefreshWithdrawsStaleProjectionUntilValidatedSnapshot() {
        val response = byteArrayOf(1)
        val rejected: (ByteArray) -> RuntimeSnapshot = { error("invalid response") }

        assertEquals(ClientState.Unavailable("RUNTIME_UNAVAILABLE"), resolveContextRefresh(null))
        assertEquals(
            ClientState.Unavailable("STORE_UNAVAILABLE"),
            resolveContextRefresh(response, rejected) { "STORE_UNAVAILABLE" },
        )
        assertEquals(
            ClientState.Unavailable("PROTOCOL_MISMATCH"),
            resolveContextRefresh(response, rejected) { null },
        )
        val restored = resolveContextRefresh(response, decode = { snapshot(hostGeneration = 9) })

        assertTrue(restored is ClientState.Available)
        assertEquals(9L, (restored as ClientState.Available).snapshot.hostGeneration)
    }

    @Test
    fun I5_G02_activationRecoveryRemainsInsideTheNativeAuthoritativeHost() {
        val nativeMethods = NativeRuntime::class.java.declaredMethods
            .filter { java.lang.reflect.Modifier.isNative(it.modifiers) }
            .map { it.name }

        assertTrue("nativeStart" in nativeMethods)
        assertTrue("nativeRecoverDeadMagiskHost" in nativeMethods)
        assertTrue("nativeSubmit" in nativeMethods)
    }

    @Test
    fun I5_G05_runtimeSessionReadersObserveOnlyWholeGenerationBoundSnapshots() {
        val inactive = RuntimeSessionState()
        val first = RuntimeSessionState(
            started = true,
            host = DaemonHostToken.ApkRuntime,
            activeFence = RuntimeFence("epoch-a", 1, "instance-a"),
            startFailure = "",
        )
        val second = RuntimeSessionState(
            started = true,
            host = DaemonHostToken.MagiskBackend,
            activeFence = RuntimeFence("epoch-a", 2, "instance-b"),
            startFailure = "",
        )
        val session = AtomicReference(inactive)
        val invalid = AtomicBoolean(false)
        val start = CountDownLatch(1)
        val pool = Executors.newFixedThreadPool(3)
        val futures = listOf(
            pool.submit {
                start.await()
                repeat(10_000) {
                    session.set(first)
                    session.compareAndSet(first, inactive)
                }
            },
            pool.submit {
                start.await()
                repeat(10_000) {
                    session.set(second)
                    session.compareAndSet(second, inactive)
                }
            },
            pool.submit {
                start.await()
                repeat(20_000) {
                    val observed = session.get()
                    if (observed.started != (observed.activeFence != null) ||
                        (observed.started && observed.startFailure.isNotEmpty())
                    ) {
                        invalid.set(true)
                    }
                }
            },
        )
        start.countDown()
        futures.forEach { it.get() }
        pool.shutdownNow()

        assertFalse(invalid.get())
        assertTrue(first.validates("epoch-a", 1, "instance-a"))
        assertFalse(first.validates("epoch-a", 2, "instance-b"))
    }

    @Test
    fun I5_G06_deadOwnerRecoveryIngressAcceptsNoCallerCleanupBoolean() {
        val recovery = NativeRuntime::class.java.getDeclaredMethod(
            "nativeRecoverDeadMagiskHost",
            String::class.java,
            String::class.java,
        )

        assertEquals(listOf(String::class.java, String::class.java), recovery.parameterTypes.toList())
        assertFalse(recovery.parameterTypes.any { it == Boolean::class.javaPrimitiveType })
    }

    @Test
    fun I5_G07_physicalFixtureBenchmarkHasOneBoundedNativeEntryPoint() {
        val benchmark = NativeRuntime::class.java.getDeclaredMethod(
            "nativeRunI5DeviceBenchmark",
            String::class.java,
        )

        assertEquals(String::class.java, benchmark.returnType)
        assertEquals(listOf(String::class.java), benchmark.parameterTypes.toList())
    }

    @Test
    fun I5_G10_retainedTransitionSelectsRecoveryBeforeNewDemotion() {
        assertEquals(
            MagiskHostStatusAction.RecoverDemotion,
            decideMagiskHostStatus(
                ready = true,
                cleanupReady = true,
                requiresApkHost = true,
                transitionPresent = true,
                pendingSourceTransition = true,
            ),
        )
        assertTrue(
            shouldRecoverUncommittedApkTransition(
                DaemonErrorToken.HostTransitionPending.wire,
                DaemonHostToken.ApkRuntime,
                "source_pending",
            ),
        )
        assertTrue(
            shouldResumeCommittedApkTransition(
                DaemonErrorToken.HostTransitionPending.wire,
                DaemonHostToken.ApkRuntime,
                "target_committed",
            ),
        )
    }

    @Test
    fun I5_G11_hostControllerPublishesOneSessionAuthorityField() {
        val fields = RuntimeHostController::class.java.declaredFields.map { it.name }

        assertTrue("runtimeSession" in fields)
        assertFalse("started" in fields)
        assertFalse("host" in fields)
        assertFalse("activeFence" in fields)
        assertFalse("startFailure" in fields)
        assertTrue(
            runCatching {
                RuntimeSessionState(
                    started = true,
                    host = DaemonHostToken.ApkRuntime,
                    activeFence = null,
                    startFailure = "",
                )
            }.isFailure,
        )
    }

    @Test
    fun I5_G04_providerFixturesHideOnlyActuallyCoveredAccessRows() {
        val grants = allUnavailable().toMutableMap().apply {
            this["shizuku.shell"] = available()
        }
        val capabilities = allCapabilitiesUnavailable().toMutableMap().apply {
            this["visual.hierarchy"] = available()
            this["visual.image"] = available()
        }
        val keys = CapabilityRows.project(snapshot(grants = grants, capabilities = capabilities)).map { it.key }

        assertTrue(CapabilityRowKey.Shizuku in keys)
        assertFalse(CapabilityRowKey.Accessibility in keys)
        assertFalse(CapabilityRowKey.ScreenCapture in keys)
        assertTrue(CapabilityRowKey.NotificationAccess in keys)
    }

    @Test
    fun I5_G04_magiskFrameworkFixtureHidesEveryCoveredAccessRow() {
        val grants = allUnavailable().toMutableMap().apply {
            this["magisk.root"] = available()
            this["magisk.module"] = available()
            this["magisk.notifications"] = available()
        }
        val capabilities = allCapabilitiesUnavailable().toMutableMap().apply {
            this["automation.persistent_time"] = available()
            this["visual.hierarchy"] = available()
            this["visual.image"] = available()
        }
        val rows = CapabilityRows.project(snapshot(grants = grants, capabilities = capabilities))

        // Magisk also covers Shizuku, so a missing Shizuku is not shown either.
        assertEquals(
            listOf(CapabilityRowKey.Runtime, CapabilityRowKey.RootBackend),
            rows.map { it.key },
        )
    }

    @Test
    fun I5_G04_lostMagiskNotificationProviderRevealsOnlyNotificationAccess() {
        val grants = allUnavailable().toMutableMap().apply {
            this["magisk.root"] = available()
            this["magisk.module"] = available()
        }
        val capabilities = allCapabilitiesUnavailable().toMutableMap().apply {
            this["automation.persistent_time"] = available()
            this["visual.hierarchy"] = available()
            this["visual.image"] = available()
        }
        val keys = CapabilityRows.project(snapshot(grants = grants, capabilities = capabilities)).map { it.key }

        assertEquals(
            listOf(
                CapabilityRowKey.Runtime,
                CapabilityRowKey.RootBackend,
                CapabilityRowKey.NotificationAccess,
            ),
            keys,
        )
    }

    @Test
    fun I5_G04_magiskLossRevealsOnlyCurrentlyMissingFunctions() {
        val beforeGrants = allUnavailable().toMutableMap().apply {
            this["magisk.root"] = available()
            this["magisk.module"] = available()
            this["magisk.notifications"] = available()
        }
        val beforeCapabilities = allCapabilitiesUnavailable().toMutableMap().apply {
            this["automation.persistent_time"] = available()
            this["visual.hierarchy"] = available()
            this["visual.image"] = available()
        }
        val after = CapabilityRows.project(
            snapshot(grants = allUnavailable(), capabilities = allCapabilitiesUnavailable()),
        ).map { it.key }
        val before = CapabilityRows.project(
            snapshot(grants = beforeGrants, capabilities = beforeCapabilities),
        ).map { it.key }

        assertEquals(
            listOf(
                CapabilityRowKey.Shizuku,
                CapabilityRowKey.LocalNetwork,
                CapabilityRowKey.NotificationAccess,
                CapabilityRowKey.ExactAlarm,
                CapabilityRowKey.Accessibility,
                CapabilityRowKey.ScreenCapture,
            ),
            after - before.toSet(),
        )
    }

    @Test
    fun I5_G04_unknownProviderFactsNeverOfferAuthorizationOrCapture() {
        val rows = CapabilityRows.project(
            snapshot(
                grants = allUnknown(),
                capabilities = capabilityKeys.associateWith { unknown() },
            ),
        )
        val conditionalRows = rows.filter { it.key !in providerKeys }

        assertTrue(conditionalRows.isNotEmpty())
        assertTrue(conditionalRows.all { it.state == CapabilityRowState.Unknown })
        assertTrue(conditionalRows.all { it.action == CapabilityAction.Recheck })
    }

    private fun registration(
        key: String,
        generation: Long,
        executor: AndroidExecutionBridge,
    ) = CapabilityRegistration(
        key = key,
        state = RegisteredCapabilityState.Available,
        reason = null,
        sourceGeneration = generation,
        executor = executor,
    )

    private fun snapshot(
        readiness: RuntimeReadiness = RuntimeReadiness.Ready,
        runtimeReason: String? = null,
        hostGeneration: Long = 1,
        grants: Map<String, AvailabilityFact> = allUnknown(),
        capabilities: Map<String, AvailabilityFact> = allCapabilitiesUnavailable(),
    ) = RuntimeSnapshot(
        sdkInt = 37,
        host = "apk_runtime",
        hostGeneration = hostGeneration,
        readiness = readiness,
        runtimeReason = runtimeReason,
        grants = grants,
        capabilities = capabilities,
    )

    private fun allUnknown() = grantKeys.associateWith { unknown() }
    private fun allUnavailable() = grantKeys.associateWith { unavailable() }
    private fun allCapabilitiesUnavailable() = capabilityKeys.associateWith { unavailable() }
    private fun available() = AvailabilityFact(AvailabilityState.Available)
    private fun unavailable() = AvailabilityFact(AvailabilityState.Unavailable, "MISSING")
    private fun unknown() = AvailabilityFact(AvailabilityState.Unknown, "ADAPTER_NOT_READY")

    private companion object {
        val grantKeys = listOf(
            "android.local_network",
            "android.notification_listener",
            "automation.exact_alarm",
            "visual.accessibility",
            "visual.media_projection_session",
            "shizuku.shell",
            "magisk.module",
            "magisk.root",
            "magisk.notifications",
        )
        val capabilityKeys = listOf("automation.persistent_time", "visual.hierarchy", "visual.image")
        val providerKeys = setOf(
            CapabilityRowKey.Runtime,
            CapabilityRowKey.RootBackend,
            CapabilityRowKey.Shizuku,
        )
    }
}
