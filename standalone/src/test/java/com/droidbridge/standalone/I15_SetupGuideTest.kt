package com.droidbridge.standalone

import com.droidbridge.ui.client.AvailabilityFact
import com.droidbridge.ui.client.AvailabilityState
import com.droidbridge.standalone.client.BackgroundFacts
import com.droidbridge.standalone.client.BackgroundKeeper
import com.droidbridge.standalone.client.BackgroundRows
import com.droidbridge.ui.client.CapabilityAction
import com.droidbridge.ui.client.CapabilityRow
import com.droidbridge.ui.client.CapabilityRowKey
import com.droidbridge.ui.client.CapabilityRowState
import com.droidbridge.ui.client.settledCapabilityStates
import com.droidbridge.standalone.client.CapabilityRows
import com.droidbridge.ui.client.RuntimeReadiness
import com.droidbridge.ui.client.SetupRoute
import com.droidbridge.ui.client.RuntimeSnapshot
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
        assertEquals(CapabilityAction.TurnOffKeepAlive, settled.first().action)
        assertTrue(settled.drop(1).all { it.action == null })
    }

    @Test
    fun keep_alive_turned_off_is_a_settled_choice_that_can_be_turned_back_on() {
        val keeper = BackgroundRows.project(openFacts, BackgroundKeeper.ShizukuOff).first()
        assertEquals(CapabilityRowKey.BackgroundKeeper, keeper.key)
        assertEquals(CapabilityRowState.KeepAliveOff, keeper.state)
        assertEquals(CapabilityAction.TurnOnKeepAlive, keeper.action)
        assertTrue(keeper.state in settledCapabilityStates)
        assertTrue(BackgroundRows.attention(openFacts, BackgroundKeeper.ShizukuOff, connectionEnabled = true).none { it.key == CapabilityRowKey.BackgroundKeeper })
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
    fun access_rows_offer_their_switch_when_nothing_can_report_them() {
        val unknown = snapshot(
            mapOf(
                "android.notification_listener" to AvailabilityFact(AvailabilityState.Unknown, "ADAPTER_NOT_READY"),
                "visual.media_projection_session" to AvailabilityFact(AvailabilityState.Unavailable, "USER_CONSENT_REQUIRED"),
            ),
        ).copy(capabilities = mapOf("visual.image" to AvailabilityFact(AvailabilityState.Unknown, null)))
        val off = CapabilityRows.project(unknown, notificationListenerGranted = false).associateBy { it.key }
        assertEquals(CapabilityAction.OpenSettings, off.getValue(CapabilityRowKey.NotificationAccess).action)
        assertEquals(CapabilityAction.OpenSettings, off.getValue(CapabilityRowKey.Accessibility).action)
        assertEquals(CapabilityAction.StartCapture, off.getValue(CapabilityRowKey.ScreenCapture).action)

        // Access is on but the listener has not connected yet: that is still being determined.
        val connecting = CapabilityRows.project(unknown, notificationListenerGranted = true).associateBy { it.key }
        assertEquals(CapabilityRowState.Unknown, connecting.getValue(CapabilityRowKey.NotificationAccess).state)
        assertEquals(CapabilityAction.Recheck, connecting.getValue(CapabilityRowKey.NotificationAccess).action)
    }

    @Test
    fun missingCompatHubOpensSetupInsteadOfReportingNoManager() {
        val rows = CapabilityRows.project(snapshot(mapOf(
            "shizuku.shell" to AvailabilityFact(AvailabilityState.Unavailable, "COMPAT_HUB_REQUIRED"),
        )), notificationListenerGranted = true)
        val row = rows.single { it.key == CapabilityRowKey.Shizuku }
        assertEquals(CapabilityRowState.CompatRequired, row.state)
        assertEquals(CapabilityAction.OpenShizuku, row.action)
        assertTrue(row.state !in settledCapabilityStates)
    }

    @Test
    fun a_stopped_shizuku_offers_to_open_it() {
        val plain = CapabilityRows.project(snapshot(), notificationListenerGranted = true).single { it.key == CapabilityRowKey.Shizuku }
        assertEquals(CapabilityAction.OpenShizuku, plain.action)
        val connected = CapabilityRows.project(snapshot(mapOf("shizuku.shell" to AvailabilityFact(AvailabilityState.Available))), notificationListenerGranted = true)
        assertEquals(CapabilityRowState.Connected, connected.single { it.key == CapabilityRowKey.Shizuku }.state)
    }

    @Test
    fun a_chosen_route_asks_for_its_own_backend_before_anything_else() {
        val background = BackgroundRows.project(openFacts, BackgroundKeeper.None)
        val access = listOf(
            CapabilityRow(CapabilityRowKey.Runtime, CapabilityRowState.Ready),
            CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.NotInstalled, CapabilityAction.InstallShizuku),
            CapabilityRow(CapabilityRowKey.Accessibility, CapabilityRowState.NotAllowed, CapabilityAction.OpenSettings),
        )

        val shizuku = CapabilityRows.steps(access, background, SetupRoute.Shizuku)
        assertEquals(listOf(CapabilityRowKey.Runtime, CapabilityRowKey.Shizuku), shizuku.access.map { it.key })
        assertTrue("the background group is judged against a backend that is not there yet", shizuku.background.isEmpty())

        val accessibility = CapabilityRows.steps(access, background, SetupRoute.AccessibilityOnly)
        assertEquals(
            listOf(CapabilityRowKey.Runtime, CapabilityRowKey.Accessibility),
            accessibility.access.map { it.key },
        )
        assertEquals(background, accessibility.background)
    }

    @Test
    fun an_answered_backend_opens_the_rest_and_a_backend_the_device_has_is_always_shown() {
        val background = BackgroundRows.project(openFacts, BackgroundKeeper.Shizuku)
        val access = listOf(
            CapabilityRow(CapabilityRowKey.Runtime, CapabilityRowState.Ready),
            CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.Connected),
            CapabilityRow(CapabilityRowKey.NotificationAccess, CapabilityRowState.NotAllowed, CapabilityAction.OpenSettings),
        )

        val shizuku = CapabilityRows.steps(access, background, SetupRoute.Shizuku)
        assertEquals(
            listOf(CapabilityRowKey.Runtime, CapabilityRowKey.Shizuku, CapabilityRowKey.NotificationAccess),
            shizuku.access.map { it.key },
        )
        assertEquals(background, shizuku.background)

        // The phone already has Shizuku, so the route that did not ask for it still reports it.
        val accessibility = CapabilityRows.steps(access, background, SetupRoute.AccessibilityOnly)
        assertEquals(
            listOf(CapabilityRowKey.Runtime, CapabilityRowKey.Shizuku, CapabilityRowKey.NotificationAccess),
            accessibility.access.map { it.key },
        )
    }

    @Test
    fun the_offered_route_is_the_one_this_phone_can_take_today() {
        assertEquals(SetupRoute.Shizuku, CapabilityRows.recommendedRoute(shizukuInstalled = true))
        assertEquals(SetupRoute.AccessibilityOnly, CapabilityRows.recommendedRoute(shizukuInstalled = false))
    }

    private fun snapshot(overrides: Map<String, AvailabilityFact> = emptyMap()): RuntimeSnapshot {
        val unavailable = AvailabilityFact(AvailabilityState.Unavailable, "FIXTURE")
        val grants = listOf("shizuku.shell", "android.notification_listener", "visual.accessibility")
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
