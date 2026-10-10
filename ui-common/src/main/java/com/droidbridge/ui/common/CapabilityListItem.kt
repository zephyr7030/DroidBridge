package com.droidbridge.ui.common

import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemColors
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.droidbridge.ui.R
import com.droidbridge.ui.client.CapabilityAction
import com.droidbridge.ui.client.CapabilityRow
import com.droidbridge.ui.client.CapabilityRowKey
import com.droidbridge.ui.client.CapabilityRowState

/** One capability fact with its next step; the page that lists it owns what the step does. */
@Composable
fun CapabilityListItem(
    row: CapabilityRow,
    refreshing: Boolean,
    colors: ListItemColors = ListItemDefaults.colors(),
    emphasized: Boolean = true,
    onAction: () -> Unit,
) {
    val action = row.action
    val reason = rowReason(row)
    val loadingDescription = stringResource(R.string.state_loading)
    ListItem(
        headlineContent = { Text(stringResource(rowTitle(row.key))) },
        supportingContent = {
            Column {
                Text(stringResource(rowState(row)))
                reason?.let { Text(stringResource(it)) }
            }
        },
        leadingContent = { Icon(painterResource(statusIcon(row.state)), contentDescription = null) },
        trailingContent = when {
            refreshing -> ({
                CircularProgressIndicator(
                    modifier = Modifier.size(24.dp).semantics { contentDescription = loadingDescription },
                )
            })
            action != null -> ({
                val tag = Modifier.testTag("cap:${rowTag(row.key)}:${action.name.lowercase()}")
                if (emphasized) {
                    Button(onClick = onAction, modifier = tag) { Text(stringResource(actionText(action))) }
                } else {
                    FilledTonalButton(onClick = onAction, modifier = tag) { Text(stringResource(actionText(action))) }
                }
            })
            else -> null
        },
        colors = colors,
        modifier = Modifier.clickable(enabled = action != null && !refreshing, onClick = onAction)
            .testTag("cap:${rowTag(row.key)}"),
    )
}

@StringRes private fun rowTitle(key: CapabilityRowKey): Int = when (key) {
    CapabilityRowKey.Runtime -> R.string.cap_runtime_title
    CapabilityRowKey.RootBackend -> R.string.cap_root_backend_title
    CapabilityRowKey.Shizuku -> R.string.cap_shizuku_title
    CapabilityRowKey.LocalNetwork -> R.string.cap_local_network_title
    CapabilityRowKey.NotificationAccess -> R.string.cap_notification_access_title
    CapabilityRowKey.ExactAlarm -> R.string.cap_exact_schedules_title
    CapabilityRowKey.Accessibility -> R.string.cap_accessibility_title
    CapabilityRowKey.ScreenCapture -> R.string.cap_screen_capture_title
    CapabilityRowKey.BackgroundKeeper -> R.string.cap_background_keeper_title
    CapabilityRowKey.BatteryOptimization -> R.string.cap_battery_optimization_title
    CapabilityRowKey.BackgroundRestriction -> R.string.cap_background_restriction_title
    CapabilityRowKey.VendorAutostart -> R.string.cap_vendor_autostart_title
    CapabilityRowKey.RecentsLock -> R.string.cap_recents_lock_title
}

@StringRes private fun rowState(row: CapabilityRow): Int = when (row.state) {
    CapabilityRowState.Ready -> if (row.key == CapabilityRowKey.BatteryOptimization) R.string.state_unrestricted else R.string.state_ready
    CapabilityRowState.Starting -> R.string.state_starting
    CapabilityRowState.Unavailable -> R.string.state_unavailable
    CapabilityRowState.NotInstalled -> when (row.key) {
        CapabilityRowKey.Shizuku -> R.string.shizuku_state_not_installed
        else -> R.string.state_not_installed
    }
    CapabilityRowState.CompatRequired -> R.string.shizuku_state_compat_required
    CapabilityRowState.UpdateRequired -> R.string.state_update_required
    CapabilityRowState.NotRunning -> R.string.shizuku_state_not_running
    CapabilityRowState.NotAuthorized -> R.string.shizuku_state_not_authorized
    CapabilityRowState.Connecting -> R.string.shizuku_state_connecting
    CapabilityRowState.Connected -> R.string.shizuku_state_connected
    CapabilityRowState.IncompatibleIdentity -> R.string.shizuku_state_incompatible_identity
    CapabilityRowState.NotAllowed -> when (row.key) {
        CapabilityRowKey.BatteryOptimization -> R.string.state_battery_optimized
        CapabilityRowKey.BackgroundRestriction -> R.string.state_background_restricted
        else -> R.string.state_not_allowed
    }
    CapabilityRowState.Active -> R.string.state_active
    CapabilityRowState.Unknown -> R.string.state_unknown
    CapabilityRowState.KeptByModule -> R.string.state_kept_by_module
    CapabilityRowState.KeptByShizuku -> R.string.state_kept_by_shizuku
    CapabilityRowState.KeepAliveOff -> R.string.state_keep_alive_off
    CapabilityRowState.NotConfirmed -> R.string.state_not_confirmed
    CapabilityRowState.Confirmed -> R.string.state_confirmed
}

@StringRes private fun actionText(action: CapabilityAction): Int = when (action) {
    CapabilityAction.Retry -> R.string.action_retry
    CapabilityAction.Recheck -> R.string.action_recheck
    CapabilityAction.Allow -> R.string.action_allow
    CapabilityAction.Diagnostics -> R.string.diagnostics_title
    CapabilityAction.InstallShizuku -> R.string.action_install_shizuku
    CapabilityAction.OpenShizuku -> R.string.action_open_shizuku
    CapabilityAction.Authorize -> R.string.action_authorize
    CapabilityAction.OpenSettings -> R.string.action_open_settings
    CapabilityAction.StartCapture -> R.string.action_start_capture
    CapabilityAction.StopCapture -> R.string.action_stop_capture
    CapabilityAction.AllowBattery -> R.string.action_allow
    CapabilityAction.OpenAppDetails -> R.string.action_open_app_details
    CapabilityAction.OpenAutostart -> R.string.action_open_settings
    CapabilityAction.ShowRecentsLockHelp -> R.string.action_show_how
    CapabilityAction.TurnOnKeepAlive -> R.string.action_turn_on
    CapabilityAction.TurnOffKeepAlive -> R.string.action_turn_off
}

@StringRes private fun rowReason(row: CapabilityRow): Int? {
    if (row.key != CapabilityRowKey.Runtime) return null
    return row.reason?.let(ReasonText::resource)
}

@DrawableRes private fun statusIcon(state: CapabilityRowState): Int = when (state) {
    CapabilityRowState.Ready, CapabilityRowState.Connected, CapabilityRowState.Active,
    CapabilityRowState.KeptByModule, CapabilityRowState.KeptByShizuku, CapabilityRowState.Confirmed -> R.drawable.ic_status_success
    CapabilityRowState.NotConfirmed, CapabilityRowState.KeepAliveOff -> R.drawable.ic_status_unknown
    CapabilityRowState.Starting, CapabilityRowState.Connecting -> R.drawable.ic_status_schedule
    CapabilityRowState.Unknown -> R.drawable.ic_status_unknown
    else -> R.drawable.ic_status_error
}

private fun rowTag(key: CapabilityRowKey): String = when (key) {
    CapabilityRowKey.Runtime -> "runtime"
    CapabilityRowKey.RootBackend -> "root_backend"
    CapabilityRowKey.Shizuku -> "shizuku"
    CapabilityRowKey.LocalNetwork -> "local_network"
    CapabilityRowKey.NotificationAccess -> "notification_access"
    CapabilityRowKey.ExactAlarm -> "exact_alarm"
    CapabilityRowKey.Accessibility -> "accessibility"
    CapabilityRowKey.ScreenCapture -> "screen_capture"
    CapabilityRowKey.BackgroundKeeper -> "background_keeper"
    CapabilityRowKey.BatteryOptimization -> "battery_optimization"
    CapabilityRowKey.BackgroundRestriction -> "background_restriction"
    CapabilityRowKey.VendorAutostart -> "vendor_autostart"
    CapabilityRowKey.RecentsLock -> "recents_lock"
}
