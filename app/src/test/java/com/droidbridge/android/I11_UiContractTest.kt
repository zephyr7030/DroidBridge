package com.droidbridge.android

import com.droidbridge.android.client.AvailabilityFact
import com.droidbridge.android.client.AvailabilityState
import com.droidbridge.android.client.CapabilityRowKey
import com.droidbridge.android.client.CapabilityRows
import com.droidbridge.android.client.RuntimeReadiness
import com.droidbridge.android.client.RuntimeSnapshot
import com.droidbridge.android.product.maintenance.MaintenanceBlocker
import com.droidbridge.android.product.maintenance.MaintenanceState
import com.droidbridge.android.ui.common.ReasonText
import com.droidbridge.android.ui.common.RouteContent
import com.droidbridge.android.ui.common.routeContent
import com.droidbridge.android.ui.common.showsRefresh
import com.droidbridge.android.ui.maintenance.reason
import com.droidbridge.android.ui.maintenance.resetAction
import com.droidbridge.android.ui.settings.DataAction
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class I11_UiContractTest {
    @Test
    fun data_actions_keep_local_mcp_and_chatgpt_credentials_separate() {
        assertTrue(DataAction.entries.contains(DataAction.ResetMcp))
        assertTrue(DataAction.entries.contains(DataAction.ClearChatGpt))
        assertFalse(DataAction.ResetMcp.tag == DataAction.ClearChatGpt.tag)
    }

    @Test
    fun I11_G01_everyAsyncRouteSharesTheCommonInitialRefreshAndErrorContract() {
        // Initial: no projection loads, and a failed first read becomes the error item.
        assertEquals(RouteContent.Loading, routeContent(hasProjection = false, loadFailed = false))
        assertEquals(RouteContent.Error, routeContent(hasProjection = false, loadFailed = true))
        assertEquals(RouteContent.Error, routeContent(hasProjection = false, loadFailed = true, empty = true))
        // Refresh and later failures keep the last immutable projection visible.
        assertEquals(RouteContent.Content, routeContent(hasProjection = true, loadFailed = true))
        assertEquals(RouteContent.Content, routeContent(hasProjection = true, loadFailed = false))
        // Only a list projection can be empty; Home and details pass no empty flag.
        assertEquals(RouteContent.Empty, routeContent(hasProjection = true, loadFailed = false, empty = true))
        assertTrue(showsRefresh(hasProjection = true, refreshing = true))
        assertFalse(showsRefresh(hasProjection = false, refreshing = true))
        assertFalse(showsRefresh(hasProjection = true, refreshing = false))
    }

    @Test
    fun I11_G03_knownReasonsUseOnlyCatalogStringsAndOthersRenderStateError() {
        val known = mapOf(
            "RUNTIME_UNAVAILABLE" to R.string.reason_runtime_unavailable,
            "MCP_LISTENER_FAILED" to R.string.reason_mcp_listener_failed,
            "PROTOCOL_MISMATCH" to R.string.reason_protocol_mismatch,
            "STORE_UNAVAILABLE" to R.string.reason_store_unavailable,
            "COMPANION_UNAVAILABLE" to R.string.reason_companion_unavailable,
            "FGS_START_REJECTED" to R.string.reason_fgs_start_rejected,
            "MODULE_CONFLICT" to R.string.reason_module_conflict,
            "USER_CONSENT_REQUIRED" to R.string.reason_user_consent_required,
            "CLEANUP_UNVERIFIED" to R.string.reason_cleanup_unverified,
        )
        known.forEach { (token, resource) -> assertEquals(token, resource, ReasonText.resource(token)) }
        for (token in listOf(null, "", "HOST_TRANSITION_PENDING", "SHIZUKU_NOT_RUNNING", "reason_runtime_unavailable")) {
            assertEquals(token.toString(), R.string.state_error, ReasonText.resource(token))
        }

        // MaintenanceRecovery never shows its blocker token, only the S-UI-017 reason resources.
        assertEquals(R.string.reason_cleanup_unverified, reason(MaintenanceState(MaintenanceBlocker.StoreCorrupt, cleanupVerified = false)))
        assertEquals(R.string.reason_store_unavailable, reason(MaintenanceState(MaintenanceBlocker.StoreCorrupt, cleanupVerified = true)))
        assertEquals(R.string.reason_runtime_unavailable, reason(MaintenanceState(MaintenanceBlocker.OwnerCorrupt, cleanupVerified = true)))
        assertEquals(R.string.action_reset_runtime_host_to_apk, resetAction(MaintenanceBlocker.OwnerCorrupt))
        assertEquals(R.string.data_reset_runtime_data, resetAction(MaintenanceBlocker.StoreCorrupt))
    }

    @Test
    fun I11_G04_coveredAccessRowsAreOmittedWithoutAReplacementRow() {
        val providerRows = listOf(CapabilityRowKey.Runtime, CapabilityRowKey.RootBackend, CapabilityRowKey.Shizuku)

        // (A) no privileged provider: every genuinely missing access row appears.
        val none = CapabilityRows.project(snapshot(grants = baseGrants(AvailabilityState.Unavailable)))
        assertEquals(
            providerRows + listOf(
                CapabilityRowKey.LocalNetwork,
                CapabilityRowKey.NotificationAccess,
                CapabilityRowKey.ExactAlarm,
                CapabilityRowKey.Accessibility,
                CapabilityRowKey.ScreenCapture,
            ),
            none.map { it.key },
        )

        // (C) Magisk covers every lower Android grant, Shizuku included: only the Runtime and root
        // rows remain, with no substitute row or reason standing in for a hidden one.
        val magisk = baseGrants(AvailabilityState.Unavailable) + mapOf(
            "magisk.root" to AvailabilityFact(AvailabilityState.Available),
            "magisk.module" to AvailabilityFact(AvailabilityState.Available),
            "magisk.notifications" to AvailabilityFact(AvailabilityState.Available),
            "magisk.wake_alarm" to AvailabilityFact(AvailabilityState.Available),
        )
        val covered = CapabilityRows.project(
            snapshot(
                grants = magisk,
                capabilities = mapOf(
                    "automation.persistent_time" to AvailabilityFact(AvailabilityState.Available),
                    "visual.image" to AvailabilityFact(AvailabilityState.Available),
                ),
            ),
        )
        assertEquals(listOf(CapabilityRowKey.Runtime, CapabilityRowKey.RootBackend), covered.map { it.key })
        assertTrue(covered.none { it.key != CapabilityRowKey.Shizuku && it.reason != null })
    }

    private fun baseGrants(state: AvailabilityState): Map<String, AvailabilityFact> = listOf(
        "android.local_network", "android.notifications", "android.notification_listener", "automation.exact_alarm",
        "visual.accessibility", "visual.media_projection_session", "shizuku.shell", "magisk.module", "magisk.root",
        "magisk.framework", "magisk.launch", "magisk.clipboard", "magisk.notifications", "magisk.wake_alarm",
        "execution.app_guard", "execution.shell_guard", "execution.root_guard",
    ).associateWith { AvailabilityFact(state, if (state == AvailabilityState.Available) null else "FIXTURE") }

    private fun snapshot(
        grants: Map<String, AvailabilityFact>,
        capabilities: Map<String, AvailabilityFact> = mapOf(
            "automation.persistent_time" to AvailabilityFact(AvailabilityState.Unavailable, "FIXTURE"),
            "visual.image" to AvailabilityFact(AvailabilityState.Unavailable, "FIXTURE"),
        ),
    ) = RuntimeSnapshot(
        sdkInt = 37,
        host = "apk_runtime",
        hostGeneration = 1,
        readiness = RuntimeReadiness.Ready,
        runtimeReason = null,
        grants = grants,
        capabilities = capabilities,
    )
}
