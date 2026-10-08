package com.droidbridge.standalone.client

import com.droidbridge.ui.client.AvailabilityState
import com.droidbridge.ui.client.CapabilityAction
import com.droidbridge.ui.client.CapabilityRow
import com.droidbridge.ui.client.CapabilityRowKey
import com.droidbridge.ui.client.CapabilityRowState
import com.droidbridge.ui.client.RuntimeSnapshot

/** Who keeps the Runtime process alive when the system ends it; ShizukuOff is the user's choice. */
enum class BackgroundKeeper { Shizuku, ShizukuOff, None }

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
    /** An unanswered keep-alive setting shows no keeper rather than a guess. */
    fun keeper(snapshot: RuntimeSnapshot?, keepAliveEnabled: Boolean?): BackgroundKeeper = when {
        snapshot?.grants?.get("shizuku.shell")?.state != AvailabilityState.Available -> BackgroundKeeper.None
        keepAliveEnabled == true -> BackgroundKeeper.Shizuku
        keepAliveEnabled == false -> BackgroundKeeper.ShizukuOff
        else -> BackgroundKeeper.None
    }

    fun project(facts: BackgroundFacts, keeper: BackgroundKeeper): List<CapabilityRow> = buildList {
        when (keeper) {
            BackgroundKeeper.Shizuku -> add(
                CapabilityRow(CapabilityRowKey.BackgroundKeeper, CapabilityRowState.KeptByShizuku, CapabilityAction.TurnOffKeepAlive),
            )
            BackgroundKeeper.ShizukuOff -> add(
                CapabilityRow(CapabilityRowKey.BackgroundKeeper, CapabilityRowState.KeepAliveOff, CapabilityAction.TurnOnKeepAlive),
            )
            BackgroundKeeper.None -> Unit
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
