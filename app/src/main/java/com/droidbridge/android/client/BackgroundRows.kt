package com.droidbridge.android.client

/** Who keeps the Runtime process alive when the system ends it. */
enum class BackgroundKeeper { MagiskModule, Shizuku, None }

/** The device facts behind the background-running group; the App reads them on every resume. */
data class BackgroundFacts(
    val batteryUnrestricted: Boolean,
    val backgroundRestricted: Boolean,
    /** The manufacturer ships its own autostart manager, so its page is offered. */
    val vendorAutostart: Boolean,
    val autostartConfirmed: Boolean,
    val recentsLockConfirmed: Boolean,
    /** The Runtime was ended by the system (not by the user) within the last day. */
    val recentSystemKill: Boolean,
)

/**
 * The background-running group of the capabilities page. It is shared by every agent connection
 * (local MCP, the ChatGPT tunnel and later ones), so it lives with the other execution facts
 * rather than on one connection's page.
 */
object BackgroundRows {
    fun keeper(snapshot: RuntimeSnapshot?): BackgroundKeeper = when {
        snapshot?.grants?.get("magisk.root")?.state == AvailabilityState.Available -> BackgroundKeeper.MagiskModule
        snapshot?.grants?.get("shizuku.shell")?.state == AvailabilityState.Available -> BackgroundKeeper.Shizuku
        else -> BackgroundKeeper.None
    }

    fun project(facts: BackgroundFacts, keeper: BackgroundKeeper): List<CapabilityRow> = buildList {
        if (keeper == BackgroundKeeper.MagiskModule) {
            // The module allowlists the App and restarts it; nothing is left for the user to do.
            add(CapabilityRow(CapabilityRowKey.BackgroundKeeper, CapabilityRowState.KeptByModule))
            return@buildList
        }
        if (keeper == BackgroundKeeper.Shizuku) {
            add(CapabilityRow(CapabilityRowKey.BackgroundKeeper, CapabilityRowState.KeptByShizuku))
        }
        add(
            if (facts.batteryUnrestricted) {
                CapabilityRow(CapabilityRowKey.BatteryOptimization, CapabilityRowState.Ready)
            } else {
                CapabilityRow(CapabilityRowKey.BatteryOptimization, CapabilityRowState.NotAllowed, CapabilityAction.AllowBattery)
            },
        )
        if (facts.backgroundRestricted) {
            add(CapabilityRow(CapabilityRowKey.BackgroundRestriction, CapabilityRowState.NotAllowed, CapabilityAction.OpenAppDetails))
        }
        if (facts.vendorAutostart) {
            add(
                if (facts.autostartConfirmed) {
                    CapabilityRow(CapabilityRowKey.VendorAutostart, CapabilityRowState.Confirmed)
                } else {
                    CapabilityRow(CapabilityRowKey.VendorAutostart, CapabilityRowState.NotConfirmed, CapabilityAction.OpenAutostart)
                },
            )
        }
        add(
            if (facts.recentsLockConfirmed) {
                CapabilityRow(CapabilityRowKey.RecentsLock, CapabilityRowState.Confirmed)
            } else {
                CapabilityRow(CapabilityRowKey.RecentsLock, CapabilityRowState.NotConfirmed, CapabilityAction.ShowRecentsLockHelp)
            },
        )
    }

    /**
     * What Home raises: nothing unless a connection is enabled. Checkable gaps always count; the
     * vendor autostart page, which cannot be read back, counts only after the system ended the
     * Runtime. The recents lock is never raised on Home.
     */
    fun attention(facts: BackgroundFacts, keeper: BackgroundKeeper, connectionEnabled: Boolean): List<CapabilityRow> {
        if (!connectionEnabled) return emptyList()
        return project(facts, keeper).filter { row ->
            when (row.key) {
                CapabilityRowKey.BatteryOptimization, CapabilityRowKey.BackgroundRestriction -> row.action != null
                CapabilityRowKey.VendorAutostart -> row.action != null && facts.recentSystemKill
                else -> false
            }
        }
    }
}
