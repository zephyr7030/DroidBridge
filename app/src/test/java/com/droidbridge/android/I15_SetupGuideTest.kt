package com.droidbridge.android

import com.droidbridge.android.client.AvailabilityFact
import com.droidbridge.android.client.AvailabilityState
import com.droidbridge.android.client.BackgroundFacts
import com.droidbridge.android.client.BackgroundKeeper
import com.droidbridge.android.client.BackgroundRows
import com.droidbridge.android.client.CapabilityAction
import com.droidbridge.android.client.CapabilityRowKey
import com.droidbridge.android.client.CapabilityRowState
import com.droidbridge.android.client.CapabilityRows
import com.droidbridge.android.client.RuntimeReadiness
import com.droidbridge.android.client.RuntimeSnapshot
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class I15_SetupGuideTest {
    private val openFacts = BackgroundFacts(
        batteryUnrestricted = false,
        backgroundRestricted = true,
        vendorAutostart = true,
        autostartConfirmed = false,
        recentsLockConfirmed = false,
        recentSystemKill = false,
    )

    @Test
    fun the_magisk_module_replaces_every_background_step() {
        val rows = BackgroundRows.project(openFacts, BackgroundKeeper.MagiskModule)
        assertEquals(listOf(CapabilityRowKey.BackgroundKeeper), rows.map { it.key })
        assertEquals(CapabilityRowState.KeptByModule, rows.single().state)
        assertTrue(BackgroundRows.attention(openFacts, BackgroundKeeper.MagiskModule, connectionEnabled = true).isEmpty())
    }

    @Test
    fun without_a_keeper_each_background_step_offers_its_own_action() {
        val rows = BackgroundRows.project(openFacts, BackgroundKeeper.None).associateBy { it.key }
        assertEquals(CapabilityAction.AllowBattery, rows.getValue(CapabilityRowKey.BatteryOptimization).action)
        assertEquals(CapabilityAction.OpenAppDetails, rows.getValue(CapabilityRowKey.BackgroundRestriction).action)
        assertEquals(CapabilityAction.OpenAutostart, rows.getValue(CapabilityRowKey.VendorAutostart).action)
        assertEquals(CapabilityAction.ShowRecentsLockHelp, rows.getValue(CapabilityRowKey.RecentsLock).action)
        assertTrue(CapabilityRowKey.BackgroundKeeper !in rows)

        val settled = BackgroundRows.project(
            openFacts.copy(batteryUnrestricted = true, backgroundRestricted = false, autostartConfirmed = true, recentsLockConfirmed = true),
            BackgroundKeeper.Shizuku,
        )
        assertEquals(CapabilityRowState.KeptByShizuku, settled.first().state)
        assertTrue(settled.all { it.action == null })
    }

    @Test
    fun home_raises_background_gaps_only_for_an_enabled_connection() {
        assertTrue(BackgroundRows.attention(openFacts, BackgroundKeeper.None, connectionEnabled = false).isEmpty())
        val quiet = BackgroundRows.attention(openFacts, BackgroundKeeper.None, connectionEnabled = true).map { it.key }
        assertEquals(listOf(CapabilityRowKey.BatteryOptimization, CapabilityRowKey.BackgroundRestriction), quiet)
        val afterKill = BackgroundRows.attention(openFacts.copy(recentSystemKill = true), BackgroundKeeper.None, connectionEnabled = true)
        assertEquals(
            listOf(CapabilityRowKey.BatteryOptimization, CapabilityRowKey.BackgroundRestriction, CapabilityRowKey.VendorAutostart),
            afterKill.map { it.key },
        )
    }

    @Test
    fun a_rooted_device_without_the_module_is_sent_to_the_module_download() {
        // No module daemon ever registers the Magisk grants, so they stay unknown.
        val silent = snapshot(mapOf("magisk.root" to AvailabilityFact(AvailabilityState.Unknown, "ADAPTER_NOT_READY")))
        assertTrue(CapabilityRows.awaitingModule(silent))
        fun rootRow(rootDetected: Boolean, moduleAbsent: Boolean) =
            CapabilityRows.project(silent, rootDetected, moduleAbsent).single { it.key == CapabilityRowKey.RootBackend }

        // Before the connect wait ends there is no verdict, so nothing is offered.
        assertEquals(CapabilityRowState.Starting, rootRow(rootDetected = true, moduleAbsent = false).state)
        val rooted = rootRow(rootDetected = true, moduleAbsent = true)
        assertEquals(CapabilityRowState.NotInstalled, rooted.state)
        assertEquals(CapabilityAction.InstallModule, rooted.action)
        val plain = rootRow(rootDetected = false, moduleAbsent = true)
        assertEquals(CapabilityRowState.Unavailable, plain.state)
        assertEquals(CapabilityRows.ROOT_NOT_DETECTED, plain.reason)
        assertNull(plain.action)
        // Once the module is known to be absent, access rows offer their switches instead of a recheck.
        val access = CapabilityRows.project(
            silent.copy(
                grants = silent.grants + mapOf(
                    "magisk.notifications" to AvailabilityFact(AvailabilityState.Unknown, "ADAPTER_NOT_READY"),
                    "android.notification_listener" to AvailabilityFact(AvailabilityState.Unknown, "ADAPTER_NOT_READY"),
                    "visual.media_projection_session" to AvailabilityFact(AvailabilityState.Unavailable, "USER_CONSENT_REQUIRED"),
                ),
                capabilities = mapOf("visual.image" to AvailabilityFact(AvailabilityState.Unknown, null)),
            ),
            rootDetected = false,
            moduleAbsent = true,
        ).associateBy { it.key }
        assertEquals(CapabilityAction.OpenSettings, access.getValue(CapabilityRowKey.NotificationAccess).action)
        assertEquals(CapabilityAction.OpenSettings, access.getValue(CapabilityRowKey.Accessibility).action)
        assertEquals(CapabilityAction.StartCapture, access.getValue(CapabilityRowKey.ScreenCapture).action)
        // A Magisk-hosted Runtime is never waiting for its module.
        assertTrue(!CapabilityRows.awaitingModule(silent.copy(host = "magisk_backend")))
    }

    @Test
    fun shizuku_is_not_asked_for_while_the_magisk_backend_is_ready() {
        val magisk = CapabilityRows.project(snapshot(mapOf("magisk.root" to AvailabilityFact(AvailabilityState.Available))))
        assertNull(magisk.firstOrNull { it.key == CapabilityRowKey.Shizuku })
        val both = CapabilityRows.project(
            snapshot(
                mapOf(
                    "magisk.root" to AvailabilityFact(AvailabilityState.Available),
                    "shizuku.shell" to AvailabilityFact(AvailabilityState.Available),
                ),
            ),
        )
        assertEquals(CapabilityRowState.Connected, both.single { it.key == CapabilityRowKey.Shizuku }.state)
        val plain = CapabilityRows.project(snapshot()).single { it.key == CapabilityRowKey.Shizuku }
        assertEquals(CapabilityAction.OpenShizuku, plain.action)
    }

    private fun snapshot(overrides: Map<String, AvailabilityFact> = emptyMap()): RuntimeSnapshot {
        val unavailable = AvailabilityFact(AvailabilityState.Unavailable, "FIXTURE")
        val grants = listOf("magisk.root", "magisk.module", "shizuku.shell", "magisk.notifications", "android.notification_listener", "visual.accessibility")
            .associateWith { unavailable } + overrides
        return RuntimeSnapshot(
            sdkInt = 36,
            host = "apk_runtime",
            hostGeneration = 1,
            readiness = RuntimeReadiness.Ready,
            runtimeReason = null,
            grants = grants,
            capabilities = emptyMap(),
        )
    }
}
