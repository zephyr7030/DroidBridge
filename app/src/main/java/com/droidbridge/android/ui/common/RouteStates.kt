package com.droidbridge.android.ui.common

import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.droidbridge.android.R

enum class RouteContent { Loading, Error, Empty, Content }

/**
 * The one S-UI-014 route-state choice every async route uses: no projection yet shows loading, a
 * failed first read shows the error item, an existing projection stays visible through refreshes
 * and later failures, and only a list route can be empty.
 */
fun routeContent(hasProjection: Boolean, loadFailed: Boolean, empty: Boolean = false): RouteContent = when {
    !hasProjection && loadFailed -> RouteContent.Error
    !hasProjection -> RouteContent.Loading
    empty -> RouteContent.Empty
    else -> RouteContent.Content
}

/** S-UI-014: the refresh indicator exists only while a refresh runs over an existing projection. */
fun showsRefresh(hasProjection: Boolean, refreshing: Boolean): Boolean = hasProjection && refreshing

/** S-UI-014: the first authoritative projection is absent, so one centered progress indicator shows. */
@Composable
fun RouteLoading(tag: String) {
    val loading = stringResource(R.string.state_loading)
    Box(Modifier.fillMaxSize().testTag("$tag:loading"), contentAlignment = Alignment.Center) {
        CircularProgressIndicator(Modifier.semantics { contentDescription = loading })
    }
}

/** S-UI-014: a refresh keeps the last projection and shows progress in the owning action area. */
@Composable
fun RefreshIndicator(tag: String) {
    val loading = stringResource(R.string.state_loading)
    CircularProgressIndicator(
        Modifier.size(24.dp).semantics { contentDescription = loading }.testTag("$tag:refreshing"),
    )
}

/** S-UI-014: a query failure with no projection; Retry exists exactly when the same read can repeat. */
@Composable
fun RouteError(tag: String, retry: (() -> Unit)?) {
    ListItem(
        headlineContent = { Text(stringResource(R.string.state_error)) },
        leadingContent = { Icon(painterResource(R.drawable.ic_status_error), contentDescription = null) },
        trailingContent = retry?.let { action ->
            {
                TextButton(onClick = action, modifier = Modifier.testTag("$tag:retry")) {
                    Text(stringResource(R.string.action_retry))
                }
            }
        },
        modifier = Modifier.testTag("$tag:error"),
    )
}

/** S-UI-014: one non-clickable empty item using the route's own empty resource. */
@Composable
fun RouteEmpty(tag: String, @StringRes text: Int) {
    ListItem(
        headlineContent = { Text(stringResource(text)) },
        leadingContent = { Icon(painterResource(R.drawable.ic_status_unknown), contentDescription = null) },
        modifier = Modifier.testTag("$tag:empty"),
    )
}

/**
 * The closed S-UI-009 ordinary-UI reason mapping. An unlisted or absent machine token renders
 * `state_error`; ordinary UI never shows the token itself.
 */
object ReasonText {
    @StringRes
    fun resource(token: String?): Int = when (token) {
        "RUNTIME_UNAVAILABLE" -> R.string.reason_runtime_unavailable
        "MCP_LISTENER_FAILED" -> R.string.reason_mcp_listener_failed
        "PROTOCOL_MISMATCH" -> R.string.reason_protocol_mismatch
        "STORE_UNAVAILABLE" -> R.string.reason_store_unavailable
        "COMPANION_UNAVAILABLE" -> R.string.reason_companion_unavailable
        "FGS_START_REJECTED" -> R.string.reason_fgs_start_rejected
        "MODULE_CONFLICT" -> R.string.reason_module_conflict
        "USER_CONSENT_REQUIRED" -> R.string.reason_user_consent_required
        "CLEANUP_UNVERIFIED" -> R.string.reason_cleanup_unverified
        else -> R.string.state_error
    }
}

/** The leading icon of a list row; the row's text already names it. */
@Composable
fun RowIcon(@DrawableRes icon: Int, modifier: Modifier = Modifier) {
    Icon(painterResource(icon), contentDescription = null, modifier = modifier)
}
