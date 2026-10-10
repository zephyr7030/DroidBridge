package com.droidbridge.standalone.client

import com.droidbridge.ui.client.AvailabilityFact
import com.droidbridge.ui.client.AvailabilityState
import com.droidbridge.ui.client.CapabilityAction
import com.droidbridge.ui.client.CapabilityRow
import com.droidbridge.ui.client.CapabilityRowKey
import com.droidbridge.ui.client.CapabilityRowState
import com.droidbridge.ui.client.RuntimeReadiness
import com.droidbridge.ui.client.RuntimeSnapshot
import com.droidbridge.ui.client.SetupRoute
import com.droidbridge.ui.client.settledCapabilityStates

/** The access and background steps of a guided setup, in the order that route asks for them. */
data class SetupSteps(val access: List<CapabilityRow>, val background: List<CapabilityRow>)

/** The capability facts of the App edition, which runs every primitive through Android and Shizuku. */
object CapabilityRows {
    /** [notificationListenerGranted]: the system setting, which the Runtime only learns from a connected listener. */
    fun project(reported: RuntimeSnapshot, notificationListenerGranted: Boolean): List<CapabilityRow> = buildList {
        val snapshot = settled(reported, notificationListenerGranted)
        add(runtime(snapshot))
        add(shizuku(snapshot))

        val localNetwork = snapshot.grant("android.local_network")
        if (snapshot.sdkInt == 37 && localNetwork.state != AvailabilityState.Available) {
            add(accessRow(CapabilityRowKey.LocalNetwork, localNetwork, CapabilityAction.Allow))
        }

        val listener = snapshot.grant("android.notification_listener")
        if (listener.state != AvailabilityState.Available) {
            add(accessRow(CapabilityRowKey.NotificationAccess, listener, CapabilityAction.OpenSettings))
        }

        val persistentTime = snapshot.capability("automation.persistent_time")
        if (persistentTime.state != AvailabilityState.Available) {
            val unavailableAction = if (
                snapshot.grant("automation.exact_alarm").state == AvailabilityState.Unavailable
            ) CapabilityAction.Allow else CapabilityAction.Recheck
            add(accessRow(CapabilityRowKey.ExactAlarm, persistentTime, unavailableAction))
        }

        val shizuku = snapshot.grant("shizuku.shell")
        val accessibility = snapshot.grant("visual.accessibility")
        val visualProviders = sufficient(shizuku, accessibility)
        if (visualProviders.state != AvailabilityState.Available) {
            add(accessRow(CapabilityRowKey.Accessibility, visualProviders, CapabilityAction.OpenSettings))
        }

        val projection = snapshot.grant("visual.media_projection_session")
        val image = snapshot.capability("visual.image")
        if (projection.state == AvailabilityState.Available) {
            add(CapabilityRow(CapabilityRowKey.ScreenCapture, CapabilityRowState.Active, CapabilityAction.StopCapture))
        } else if (image.state != AvailabilityState.Available) {
            add(accessRow(CapabilityRowKey.ScreenCapture, image, CapabilityAction.StartCapture))
        }
    }

    /** The route this phone can take today, offered first while the user has not chosen one. */
    fun recommendedRoute(shizukuInstalled: Boolean): SetupRoute =
        if (shizukuInstalled) SetupRoute.Shizuku else SetupRoute.AccessibilityOnly

    /**
     * What the chosen route asks for during first setup. Shizuku is judged first when it is the
     * chosen backend, since every other step depends on whether it answers; once it does, or for
     * the accessibility route, every step is shown.
     */
    fun steps(access: List<CapabilityRow>, background: List<CapabilityRow>, route: SetupRoute): SetupSteps {
        val gate = when (route) {
            SetupRoute.Shizuku -> CapabilityRowKey.Shizuku
            SetupRoute.AccessibilityOnly -> null
        }
        val chosen = access.filter {
            it.key != CapabilityRowKey.Shizuku || it.key == gate || it.state in settledCapabilityStates
        }
        val waiting = chosen.any { it.key == gate && it.state !in settledCapabilityStates }
        return if (waiting) {
            SetupSteps(chosen.filter { it.key == CapabilityRowKey.Runtime || it.key == gate }, emptyList())
        } else {
            SetupSteps(chosen, background)
        }
    }

    /**
     * A listener whose access is off never connects, so it never registers its fact; and screen
     * images stay unknown while no capture session exists. Both are then the switch the user can
     * turn on, not "unknown, recheck".
     */
    private fun settled(snapshot: RuntimeSnapshot, notificationListenerGranted: Boolean): RuntimeSnapshot {
        fun settle(facts: Map<String, AvailabilityFact>, key: String, absent: Boolean) = facts.mapValues { (name, fact) ->
            if (absent && name == key && fact.state == AvailabilityState.Unknown) AvailabilityFact(AvailabilityState.Unavailable, fact.reason) else fact
        }
        val noCapture = snapshot.grant("visual.media_projection_session").state == AvailabilityState.Unavailable
        return snapshot.copy(
            grants = settle(snapshot.grants, "android.notification_listener", !notificationListenerGranted),
            capabilities = settle(snapshot.capabilities, "visual.image", noCapture),
        )
    }

    private fun runtime(snapshot: RuntimeSnapshot): CapabilityRow = when (snapshot.readiness) {
        RuntimeReadiness.Ready -> CapabilityRow(CapabilityRowKey.Runtime, CapabilityRowState.Ready)
        RuntimeReadiness.Initializing -> CapabilityRow(CapabilityRowKey.Runtime, CapabilityRowState.Starting)
        RuntimeReadiness.Unavailable -> CapabilityRow(
            CapabilityRowKey.Runtime,
            CapabilityRowState.Unavailable,
            runtimeAction(snapshot.runtimeReason),
            snapshot.runtimeReason,
        )
    }

    fun runtimeAction(reason: String?): CapabilityAction = when (reason) {
        "CLEANUP_UNVERIFIED", "PROTOCOL_MISMATCH" -> CapabilityAction.Diagnostics
        else -> CapabilityAction.Retry
    }

    private fun shizuku(snapshot: RuntimeSnapshot): CapabilityRow {
        val fact = snapshot.grant("shizuku.shell")
        return when {
            fact.state == AvailabilityState.Available -> CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.Connected)
            fact.reason == "COMPAT_HUB_REQUIRED" -> CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.CompatRequired, CapabilityAction.OpenShizuku)
            fact.reason == "MANAGER_NOT_INSTALLED" -> CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.NotInstalled, CapabilityAction.InstallShizuku)
            fact.reason == "BINDER_UNAVAILABLE" -> CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.NotRunning, CapabilityAction.OpenShizuku)
            fact.reason == "GRANT_MISSING" -> CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.NotAuthorized, CapabilityAction.Authorize)
            fact.reason == "INCOMPATIBLE_IDENTITY" -> CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.IncompatibleIdentity)
            fact.state == AvailabilityState.Unknown -> CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.Connecting)
            else -> CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.NotRunning, CapabilityAction.OpenShizuku)
        }
    }

    private fun accessRow(
        key: CapabilityRowKey,
        fact: AvailabilityFact,
        unavailableAction: CapabilityAction,
    ): CapabilityRow = when (fact.state) {
        AvailabilityState.Unknown -> CapabilityRow(key, CapabilityRowState.Unknown, CapabilityAction.Recheck, fact.reason)
        AvailabilityState.Unavailable -> CapabilityRow(key, CapabilityRowState.NotAllowed, unavailableAction, fact.reason)
        AvailabilityState.Available -> error("available access rows are hidden")
    }

    private fun sufficient(vararg facts: AvailabilityFact): AvailabilityFact {
        if (facts.any { it.state == AvailabilityState.Available }) return AvailabilityFact(AvailabilityState.Available)
        if (facts.all { it.state == AvailabilityState.Unavailable }) return AvailabilityFact(AvailabilityState.Unavailable)
        return AvailabilityFact(AvailabilityState.Unknown)
    }

    private fun RuntimeSnapshot.grant(key: String): AvailabilityFact =
        grants[key] ?: AvailabilityFact(AvailabilityState.Unknown, "MISSING_FACT")

    private fun RuntimeSnapshot.capability(key: String): AvailabilityFact =
        capabilities[key] ?: AvailabilityFact(AvailabilityState.Unknown, "MISSING_FACT")
}
