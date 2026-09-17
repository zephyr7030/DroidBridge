package com.droidbridge.android

import com.droidbridge.android.execution.android.AndroidPrimitive
import com.droidbridge.android.execution.android.RegisteredCapabilityState
import com.droidbridge.android.execution.shizuku.ExecutionOwnershipRegistry
import com.droidbridge.android.execution.shizuku.ShizukuCapabilityProjector
import com.droidbridge.android.execution.shizuku.ShizukuObservation
import com.droidbridge.android.execution.shizuku.ShizukuPackageParser
import com.droidbridge.android.execution.shizuku.ShizukuCapabilityPublisher
import com.droidbridge.android.execution.shizuku.ShizukuProviderState
import com.droidbridge.android.execution.shizuku.ShizukuLaunchPolicy
import com.droidbridge.android.execution.shizuku.ShizukuGuardedPlanCodec
import com.droidbridge.android.execution.shizuku.ShizukuFsCodec
import com.droidbridge.android.execution.shizuku.ShizukuController
import com.droidbridge.android.execution.shizuku.ShizukuProbeSettlement
import com.droidbridge.android.execution.shizuku.ShizukuPrimitivePolicy
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class I6_GatesTest {
    @Test
    fun I6_G01_guardProbePublicationDoesNotReplaceShizukuExecutorGeneration() {
        val registrations = mutableListOf<Pair<String, Long>>()
        val registry = com.droidbridge.android.execution.android.AndroidExecutionRegistry(
            registrationSink = { key, _, _, generation, _ ->
                registrations += key to generation
                true
            },
        )
        var generation = 100L
        val publisher = ShizukuCapabilityPublisher(registry) { ++generation }

        publisher.publishProvider(
            ShizukuObservation(true, true, true, 2_000, false),
            executor = null,
        )
        publisher.publishGuard(RegisteredCapabilityState.Unknown, "CONNECTING")
        publisher.publishGuard(RegisteredCapabilityState.Available, null)

        assertEquals(listOf(101L), registrations.filter { it.first == "shizuku.shell" }.map { it.second })
        assertEquals(
            listOf(102L, 103L),
            registrations.filter { it.first == "execution.shell_guard" }.map { it.second },
        )
    }

    @Test
    fun I6_G01_providerIdentityProjectionNeverPromotesMissingGrantOrUid0() {
        val fixtures = listOf(
            ShizukuObservation(false, false, false, null, false) to
                Triple(ShizukuProviderState.NotInstalled, RegisteredCapabilityState.Unavailable, "MANAGER_NOT_INSTALLED"),
            ShizukuObservation(true, false, false, null, false) to
                Triple(ShizukuProviderState.NotRunning, RegisteredCapabilityState.Unavailable, "BINDER_UNAVAILABLE"),
            ShizukuObservation(true, true, false, null, false) to
                Triple(ShizukuProviderState.NotAuthorized, RegisteredCapabilityState.Unavailable, "GRANT_MISSING"),
            ShizukuObservation(true, true, true, null, false, userServiceFailed = true) to
                Triple(ShizukuProviderState.NotRunning, RegisteredCapabilityState.Unavailable, "BINDER_UNAVAILABLE"),
            ShizukuObservation(true, true, true, null, true) to
                Triple(ShizukuProviderState.Connecting, RegisteredCapabilityState.Unknown, "CONNECTING"),
            ShizukuObservation(true, true, true, 2_000, false) to
                Triple(ShizukuProviderState.Connected, RegisteredCapabilityState.Available, null),
            ShizukuObservation(true, true, true, 0, false) to
                Triple(ShizukuProviderState.IncompatibleIdentity, RegisteredCapabilityState.Unavailable, "INCOMPATIBLE_IDENTITY"),
        )

        fixtures.forEach { (observation, expected) ->
            val actual = ShizukuCapabilityProjector.project(observation)
            assertEquals(expected.first, actual.providerState)
            assertEquals(expected.second, actual.registrationState)
            assertEquals(expected.third, actual.reason)
            assertEquals(expected.second == RegisteredCapabilityState.Available, actual.exposesExecutor)
        }

        val cleanupQuarantined = ShizukuCapabilityProjector.project(
            ShizukuObservation(
                managerInstalled = true,
                binderAlive = true,
                authorized = true,
                userServiceUid = 2_000,
                connecting = false,
                cleanupQuarantined = true,
            ),
        )
        assertEquals(ShizukuProviderState.Connected, cleanupQuarantined.providerState)
        assertEquals(RegisteredCapabilityState.Available, cleanupQuarantined.registrationState)
        assertFalse(cleanupQuarantined.exposesExecutor)
    }

    @Test
    fun I6_G02_controllerOwnsSessionOrchestrationButNoToolPrimitives() {
        val methods = ShizukuController::class.java.declaredMethods.map { it.name }.toSet()

        assertTrue(setOf("start", "stop", "recheck", "requestAuthorization").all(methods::contains))
        assertTrue(
            setOf(
                "executeProcess",
                "executeGuarded",
                "executePackage",
                "executeFs",
                "packageList",
                "packageInspect",
                "packageForceStop",
                "packageInventory",
                "packageInventoryPart",
                "prepareProof",
                "settleProof",
                "createGuardIo",
                "readBounded",
            ).none(methods::contains),
        )
    }

    @Test
    fun I6_G03_daemonAdapterCanRebindWithoutOwningBusinessState() {
        val registry = ExecutionOwnershipRegistry<String, String>(64)
        registry.attach("client-a")
        assertTrue(registry.admit("client-a", "execution-a", "handle-a"))
        assertEquals(setOf("handle-a"), registry.detach("client-a"))

        registry.attach("client-b")
        assertTrue(registry.admit("client-b", "execution-b", "handle-b"))
        assertEquals(1, registry.activeCount)
        assertEquals("handle-b", registry.ownedHandle("client-b", "execution-b"))
    }

    @Test
    fun I6_G04_clientLossReturnsEveryOwnedHandleForCleanup() {
        val registry = ExecutionOwnershipRegistry<String, Long>(64)
        registry.attach("client")
        assertTrue(registry.admit("client", "execution-1", 11L))
        assertTrue(registry.admit("client", "execution-2", 12L))

        assertEquals(setOf(11L, 12L), registry.detach("client"))
        assertEquals(0, registry.activeCount)
        assertFalse(registry.admit("client", "execution-3", 13L))
    }

    @Test
    fun I6_G05_laterClientCannotAdoptOrCancelOldExecutionIdentity() {
        val registry = ExecutionOwnershipRegistry<String, Long>(64)
        registry.attach("old")
        assertTrue(registry.admit("old", "same-execution-id", 21L))
        assertEquals(setOf(21L), registry.detach("old"))
        registry.attach("new")

        assertEquals(null, registry.ownedHandle("new", "same-execution-id"))
        assertEquals(null, registry.remove("new", "same-execution-id"))
        assertTrue(registry.admit("new", "new-execution-id", 22L))
    }

    @Test
    fun I6_G06_guardLaunchPolicyRejectsRootWrongPathAndFalseCancellation() {
        val nativeDirectory = "/data/app/example/lib/arm64"
        val guard = "$nativeDirectory/libdroidbridge_exec_guard.so"
        assertTrue(ShizukuLaunchPolicy.isAdmitted(2_000, 10_123, 10_123, nativeDirectory, guard, 63))
        assertFalse(ShizukuLaunchPolicy.isAdmitted(0, 10_123, 10_123, nativeDirectory, guard, 0))
        assertFalse(ShizukuLaunchPolicy.isAdmitted(2_000, 10_124, 10_123, nativeDirectory, guard, 0))
        assertFalse(
            ShizukuLaunchPolicy.isAdmitted(
                2_000,
                10_123,
                10_123,
                nativeDirectory,
                "/data/local/tmp/libdroidbridge_exec_guard.so",
                0,
            ),
        )
        assertFalse(ShizukuLaunchPolicy.isAdmitted(2_000, 10_123, 10_123, nativeDirectory, guard, 64))

        assertEquals(ShizukuProbeSettlement.Available, ShizukuLaunchPolicy.settleProbe(true, 0))
        assertEquals(ShizukuProbeSettlement.Unavailable, ShizukuLaunchPolicy.settleProbe(true, 4))
        assertEquals(ShizukuProbeSettlement.CleanupUnverified, ShizukuLaunchPolicy.settleProbe(false, null))
        assertFalse(
            ShizukuPrimitivePolicy.isAdmitted(
                AndroidPrimitive.ShizukuProcessStart,
                RegisteredCapabilityState.Unknown,
                cleanupQuarantined = false,
            ),
        )
        assertFalse(
            ShizukuPrimitivePolicy.isAdmitted(
                AndroidPrimitive.ShizukuPackagePrimitive,
                RegisteredCapabilityState.Unavailable,
                cleanupQuarantined = false,
            ),
        )
        assertTrue(
            ShizukuPrimitivePolicy.isAdmitted(
                AndroidPrimitive.ShizukuFsPrimitive,
                RegisteredCapabilityState.Unavailable,
                cleanupQuarantined = false,
            ),
        )
        assertFalse(
            ShizukuPrimitivePolicy.isAdmitted(
                AndroidPrimitive.ShizukuFsPrimitive,
                RegisteredCapabilityState.Available,
                cleanupQuarantined = true,
            ),
        )
        assertTrue(
            ShizukuPrimitivePolicy.isAdmitted(
                AndroidPrimitive.ShizukuProcessCancel,
                RegisteredCapabilityState.Unavailable,
                cleanupQuarantined = true,
            ),
        )
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuPackageParser.parse(listOf("package:com.example versionCode:not-a-number"), false)
        }
        assertEquals(
            listOf("com.alpha" to 7L, "com.example" to 42L),
            ShizukuPackageParser.parse(
                listOf("package:com.example versionCode:42", "package:com.alpha versionCode:7"),
                false,
            ).map { it.packageName to it.versionCode.toLong() },
        )
        assertEquals(
            "18446744073709551615",
            ShizukuPackageParser.parse(
                listOf("package:com.max versionCode:18446744073709551615"),
                false,
            ).single().versionCode.toString(),
        )
        assertEquals(
            listOf("cmd", "package", "list", "packages", "-3", "--show-versioncode", "--user", "0"),
            ShizukuGuardedPlanCodec.decode(
                "package_list_third_party",
                "{}".toByteArray(),
                guard,
            ).let { listOf(it.program.substringAfterLast('/')) + it.arguments },
        )
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuGuardedPlanCodec.decode(
                "package_list_third_party",
                "{\"flag\":\"--user 10\"}".toByteArray(),
                guard,
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuGuardedPlanCodec.decode(
                "process_start",
                byteArrayOf(0xc3.toByte(), 0x28),
                guard,
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuGuardedPlanCodec.decode(
                "process_start",
                "{\"program\":7,\"arguments\":[],\"timeout_ms\":1000}".toByteArray(),
                guard,
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuGuardedPlanCodec.decode(
                "process_start",
                "{\"program\":\"/system/bin/id\",\"arguments\":[false],\"timeout_ms\":1000}"
                    .toByteArray(),
                guard,
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuGuardedPlanCodec.decode(
                "process_start",
                "{\"program\":\"/system/bin/id\",\"arguments\":[],\"timeout_ms\":\"1000\"}"
                    .toByteArray(),
                guard,
            )
        }
        assertEquals(
            "/system/bin/screencap",
            ShizukuGuardedPlanCodec.decodeInvocation(
                "{\"operation\":\"screen_capture\"}".toByteArray(),
                guard,
            ).plan.program,
        )
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuFsCodec.decode(
                "{\"operation\":\"rename_same_directory\",\"source\":\"/data/local/tmp/a\",\"destination\":\"/data/local/b\"}"
                    .toByteArray(),
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuFsCodec.decode(
                "{\"operation\":\"create_exclusive\",\"path\":\"/data/local/tmp/a\",\"mode\":\"384\"}"
                    .toByteArray(),
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuFsCodec.decode(byteArrayOf(0xc3.toByte(), 0x28))
        }
        assertTrue(
            ShizukuFsCodec.decode(
                "{\"operation\":\"apply_metadata\",\"uid\":2000,\"gid\":2000,\"mode\":384,\"selinux_context_base64\":\"dTpvOnQ6czA6AA==\"}"
                    .toByteArray(),
            ) is com.droidbridge.android.execution.shizuku.ShizukuFsOperation.ApplyMetadata,
        )
        assertTrue(
            ShizukuFsCodec.decode(
                "{\"operation\":\"rename_atomic\",\"source\":\"/a\",\"destination\":\"/b\",\"exchange\":false}"
                    .toByteArray(),
            ) is com.droidbridge.android.execution.shizuku.ShizukuFsOperation.RenameAtomic,
        )
        assertEquals(
            com.droidbridge.android.execution.shizuku.ShizukuFsOperation.ReadDirectory(
                "/data/local/tmp",
                17,
                512,
            ),
            ShizukuFsCodec.decode(
                "{\"operation\":\"read_directory\",\"path\":\"/data/local/tmp\",\"cookie\":17,\"limit\":512}"
                    .toByteArray(),
            ),
        )
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuFsCodec.decode(
                "{\"operation\":\"read_directory\",\"path\":\"/data/local/tmp\",\"cookie\":-1,\"limit\":512}"
                    .toByteArray(),
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            ShizukuFsCodec.decode(
                "{\"operation\":\"read_directory\",\"path\":\"/data/local/tmp\",\"cookie\":0,\"limit\":5002}"
                    .toByteArray(),
            )
        }
        assertEquals(
            com.droidbridge.android.execution.shizuku.ShizukuFsOperation.ReadBounded(
                "/proc/net/tcp",
                ShizukuFsCodec.MAX_READ_BOUNDED_BYTES,
            ),
            ShizukuFsCodec.decode(
                "{\"operation\":\"read_bounded\",\"path\":\"/proc/net/tcp\",\"limit\":262144}"
                    .toByteArray(),
            ),
        )
        for (invalid in listOf(
            "{\"operation\":\"read_bounded\",\"path\":\"/proc/net/tcp\",\"limit\":0}",
            "{\"operation\":\"read_bounded\",\"path\":\"/proc/net/tcp\",\"limit\":262145}",
            "{\"operation\":\"read_bounded\",\"path\":\"/proc/net/../tcp\",\"limit\":1}",
            "{\"operation\":\"read_bounded\",\"path\":\"/proc/net/tcp\"}",
            "{\"operation\":\"read_bounded\",\"path\":\"/proc/net/tcp\",\"limit\":1,\"extra\":1}",
        )) {
            assertThrows(IllegalArgumentException::class.java) {
                ShizukuFsCodec.decode(invalid.toByteArray())
            }
        }
    }

    @Test
    fun I6_G07_toolResponsibilitiesHaveDedicatedExecutorOwners() {
        listOf(
            "com.droidbridge.android.execution.shizuku.ShizukuGuardExecutor",
            "com.droidbridge.android.execution.shizuku.ShizukuProcessExecutor",
            "com.droidbridge.android.execution.shizuku.ShizukuPackageExecutor",
            "com.droidbridge.android.execution.shizuku.ShizukuFsClientExecutor",
        ).forEach { className ->
            assertTrue(Class.forName(className).declaredMethods.isNotEmpty())
        }
    }
}
