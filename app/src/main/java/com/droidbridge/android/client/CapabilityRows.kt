package com.droidbridge.android.client

enum class CapabilityRowKey {
    Runtime,
    RootBackend,
    Shizuku,
    LocalNetwork,
    NotificationAccess,
    ExactAlarm,
    Accessibility,
    ScreenCapture,
    BackgroundKeeper,
    BatteryOptimization,
    BackgroundRestriction,
    VendorAutostart,
    RecentsLock,
}

enum class CapabilityRowState {
    Ready,
    Starting,
    Unavailable,
    NotInstalled,
    UpdateRequired,
    Conflict,
    NotRunning,
    NotAuthorized,
    Connecting,
    Connected,
    IncompatibleIdentity,
    NotAllowed,
    Active,
    Unknown,
    KeptByModule,
    KeptByShizuku,
    NotConfirmed,
    Confirmed,
}

enum class CapabilityAction {
    Retry,
    Recheck,
    Allow,
    Diagnostics,
    InstallModule,
    UpdateModule,
    InstallShizuku,
    OpenShizuku,
    Authorize,
    OpenSettings,
    StartCapture,
    StopCapture,
    AllowBattery,
    OpenAppDetails,
    OpenAutostart,
    ShowRecentsLockHelp,
}

data class CapabilityRow(
    val key: CapabilityRowKey,
    val state: CapabilityRowState,
    val action: CapabilityAction? = null,
    val reason: String? = null,
)

/** A row in one of these states asks the user for nothing. */
val settledCapabilityStates = setOf(
    CapabilityRowState.Ready,
    CapabilityRowState.Connected,
    CapabilityRowState.Active,
    CapabilityRowState.KeptByModule,
    CapabilityRowState.KeptByShizuku,
    CapabilityRowState.Confirmed,
)

/**
 * How the user chose to let DroidBridge act on this phone, picked once during first setup. It
 * shapes the setup guide only: afterwards the capabilities page reports every fact the device
 * has, since a stronger backend may be installed long after setup.
 */
enum class SetupRoute(val wireValue: String) {
    RootModule("root_module"),
    Shizuku("shizuku"),
    AccessibilityOnly("accessibility_only"),
}

/** The access and background steps of a guided setup, in the order that route asks for them. */
data class SetupSteps(val access: List<CapabilityRow>, val background: List<CapabilityRow>)

object CapabilityRows {
    /**
     * [rootDetected]: a root manager or `su` is present on the device, found without asking `su`.
     * [moduleAbsent]: the Runtime has been ready long enough for a module daemon to connect, and none did.
     */
    fun project(reported: RuntimeSnapshot, rootDetected: Boolean = false, moduleAbsent: Boolean = false): List<CapabilityRow> = buildList {
        add(runtime(reported))
        add(root(reported, rootDetected, moduleAbsent))
        // Without a module the Magisk facts never settle; waiting on them would leave every access
        // row at "unknown, recheck" instead of offering the switch the user can turn on.
        val snapshot = if (moduleAbsent) withoutModule(reported) else reported
        val magiskRoot = snapshot.grant("magisk.root")
        // The Magisk backend covers every Shizuku capability, so a missing Shizuku is not shown then,
        // the same way the access rows below hide once a backend covers them.
        if (magiskRoot.state != AvailabilityState.Available || snapshot.grant("shizuku.shell").state == AvailabilityState.Available) {
            add(shizuku(snapshot))
        }

        val localNetwork = snapshot.grant("android.local_network")
        if (snapshot.sdkInt == 37 && magiskRoot.state != AvailabilityState.Available && localNetwork.state != AvailabilityState.Available) {
            add(accessRow(CapabilityRowKey.LocalNetwork, sufficient(magiskRoot, localNetwork), CapabilityAction.Allow))
        }

        val magiskNotifications = snapshot.grant("magisk.notifications")
        val listener = snapshot.grant("android.notification_listener")
        if (magiskNotifications.state != AvailabilityState.Available && listener.state != AvailabilityState.Available) {
            add(accessRow(CapabilityRowKey.NotificationAccess, sufficient(magiskNotifications, listener), CapabilityAction.OpenSettings))
        }

        val persistentTime = snapshot.capability("automation.persistent_time")
        if (persistentTime.state != AvailabilityState.Available) {
            val unavailableAction = if (
                snapshot.host == "apk_runtime" &&
                snapshot.grant("automation.exact_alarm").state == AvailabilityState.Unavailable
            ) CapabilityAction.Allow else CapabilityAction.Recheck
            add(accessRow(CapabilityRowKey.ExactAlarm, persistentTime, unavailableAction))
        }

        val shizuku = snapshot.grant("shizuku.shell")
        val accessibility = snapshot.grant("visual.accessibility")
        val visualProviders = sufficient(magiskRoot, shizuku, accessibility)
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

    const val ROOT_NOT_DETECTED = "ROOT_NOT_DETECTED"

    /** The route this phone can take today, offered first while the user has not chosen one. */
    fun recommendedRoute(rootDetected: Boolean, shizukuInstalled: Boolean): SetupRoute = when {
        rootDetected -> SetupRoute.RootModule
        shizukuInstalled -> SetupRoute.Shizuku
        else -> SetupRoute.AccessibilityOnly
    }

    /**
     * What the chosen route asks for during first setup. The backend another route would set up is
     * not this route's work, and every other step is judged against a backend that is not there
     * yet, so the route's own backend is asked for alone until it answers. A backend the device
     * already has is always shown: that is what it reports, whatever was chosen.
     */
    fun steps(access: List<CapabilityRow>, background: List<CapabilityRow>, route: SetupRoute): SetupSteps {
        val gate = when (route) {
            SetupRoute.RootModule -> CapabilityRowKey.RootBackend
            SetupRoute.Shizuku -> CapabilityRowKey.Shizuku
            SetupRoute.AccessibilityOnly -> null
        }
        val backends = setOf(CapabilityRowKey.RootBackend, CapabilityRowKey.Shizuku)
        val chosen = access.filter {
            it.key !in backends || it.key == gate || it.state in settledCapabilityStates
        }
        val waiting = chosen.any { it.key == gate && it.state !in settledCapabilityStates }
        return if (waiting) {
            SetupSteps(chosen.filter { it.key == CapabilityRowKey.Runtime || it.key == gate }, emptyList())
        } else {
            SetupSteps(chosen, background)
        }
    }

    private fun withoutModule(snapshot: RuntimeSnapshot): RuntimeSnapshot {
        fun settle(facts: Map<String, AvailabilityFact>, keys: (String) -> Boolean) = facts.mapValues { (key, fact) ->
            if (keys(key) && fact.state == AvailabilityState.Unknown) AvailabilityFact(AvailabilityState.Unavailable, "MODULE_ABSENT") else fact
        }
        return snapshot.copy(
            // A notification listener that was never enabled never registers either.
            grants = settle(snapshot.grants) { it.startsWith("magisk.") || it == "android.notification_listener" },
            capabilities = settle(snapshot.capabilities) { it == "visual.image" },
        )
    }

    /** No module daemon has connected to an App-hosted, ready Runtime; [moduleAbsent] adds the wait. */
    fun awaitingModule(snapshot: RuntimeSnapshot?): Boolean =
        snapshot != null && snapshot.host == "apk_runtime" && snapshot.readiness == RuntimeReadiness.Ready &&
            snapshot.grant("magisk.root").state == AvailabilityState.Unknown

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

    private fun runtimeAction(reason: String?): CapabilityAction = when (reason) {
        "CLEANUP_UNVERIFIED", "PROTOCOL_MISMATCH", "MODULE_CONFLICT" -> CapabilityAction.Diagnostics
        else -> CapabilityAction.Retry
    }

    private fun root(snapshot: RuntimeSnapshot, rootDetected: Boolean, moduleAbsent: Boolean): CapabilityRow {
        val root = snapshot.grant("magisk.root")
        val module = snapshot.grant("magisk.module")
        if (root.state == AvailabilityState.Available) {
            return CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.Ready)
        }
        return when (module.reason) {
            "MODULE_NOT_INSTALLED" -> CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.NotInstalled, CapabilityAction.InstallModule)
            "MODULE_UPDATE_REQUIRED" -> CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.UpdateRequired, CapabilityAction.UpdateModule)
            "MODULE_CONFLICT" -> CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.Conflict, CapabilityAction.Diagnostics)
            "MODULE_EXCLUDED" -> CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.Unavailable, CapabilityAction.UpdateModule)
            else -> if (moduleAbsent && root.state == AvailabilityState.Unknown) {
                if (rootDetected) {
                    // Rooted, but no module daemon ever connected: the module is missing or disabled.
                    CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.NotInstalled, CapabilityAction.InstallModule)
                } else {
                    // An ordinary phone: stated once, never raised as something to fix.
                    CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.Unavailable, reason = ROOT_NOT_DETECTED)
                }
            } else if (root.state == AvailabilityState.Unknown || module.state == AvailabilityState.Unknown) {
                CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.Starting)
            } else {
                CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.Unavailable, CapabilityAction.Recheck)
            }
        }
    }

    private fun shizuku(snapshot: RuntimeSnapshot): CapabilityRow {
        val fact = snapshot.grant("shizuku.shell")
        return when {
            fact.state == AvailabilityState.Available -> CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.Connected)
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
