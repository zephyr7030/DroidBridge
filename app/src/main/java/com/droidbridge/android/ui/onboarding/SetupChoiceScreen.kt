package com.droidbridge.android.ui.onboarding

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.droidbridge.android.R
import com.droidbridge.android.client.SetupRoute
import com.droidbridge.android.product.settings.AgentType
import com.droidbridge.android.ui.settings.BackButton

/** What this phone already has, so the route it can use today is the one offered first. */
data class SetupChoiceFacts(val rootDetected: Boolean, val shizukuInstalled: Boolean)

private data class Option<T>(
    val value: T,
    @StringRes val title: Int,
    @StringRes val body: Int,
    val available: Boolean = true,
)

private val routeOptions = listOf(
    Option(SetupRoute.RootModule, R.string.route_root_module, R.string.route_root_module_body),
    Option(SetupRoute.Shizuku, R.string.route_shizuku, R.string.route_shizuku_body),
    Option(SetupRoute.AccessibilityOnly, R.string.route_accessibility, R.string.route_accessibility_body),
)

private val agentOptions = listOf(
    Option(AgentType.LocalMcp, R.string.agent_local_mcp, R.string.agent_local_mcp_body),
    Option(AgentType.ChatGpt, R.string.agent_chatgpt, R.string.agent_chatgpt_body),
    Option(AgentType.Other, R.string.agent_other, R.string.agent_other_body, available = false),
)

/**
 * First-launch choice of how DroidBridge acts on this phone and which agent reaches it. Both are
 * chosen here because the guide that follows is built from the pair: the environment is set up
 * first, then the agent that uses it.
 */
@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun SetupChoiceRoute(
    route: SetupRoute,
    selectRoute: (SetupRoute) -> Unit,
    agent: AgentType,
    selectAgent: (AgentType) -> Unit,
    facts: SetupChoiceFacts,
    continueSetup: () -> Unit,
    back: () -> Unit,
) {
    Scaffold(
        modifier = Modifier.testTag("route:SetupChoice"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.setup_choice_title)) },
                navigationIcon = { BackButton("route:SetupChoice", R.string.setup_choice_title, back) },
            )
        },
        bottomBar = {
            Button(
                onClick = continueSetup,
                enabled = agentOptions.first { it.value == agent }.available,
                modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp).heightIn(min = 56.dp)
                    .testTag("setup_choice:continue"),
            ) { Text(stringResource(R.string.setup_choice_start)) }
        },
    ) { padding ->
        Column(
            modifier = Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.setup_choice_description),
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            SectionTitle(R.string.setup_section_environment)
            routeOptions.forEach { option ->
                ChoiceCard(
                    option = option,
                    selected = option.value == route,
                    tag = "setup_choice:${option.value.wireValue}",
                    badge = when {
                        option.value == SetupRoute.RootModule && facts.rootDetected -> R.string.route_badge_root
                        option.value == SetupRoute.Shizuku && facts.shizukuInstalled -> R.string.route_badge_installed
                        else -> null
                    },
                ) { selectRoute(option.value) }
            }
            SectionTitle(R.string.setup_section_agent)
            agentOptions.forEach { option ->
                ChoiceCard(
                    option = option,
                    selected = option.value == agent,
                    tag = "setup_choice:${option.value.wireValue}",
                    badge = null,
                ) { selectAgent(option.value) }
            }
        }
    }
}

@Composable
private fun SectionTitle(@StringRes title: Int) {
    Text(
        stringResource(title),
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(top = 8.dp),
    )
}

@Composable
private fun <T> ChoiceCard(
    option: Option<T>,
    selected: Boolean,
    tag: String,
    @StringRes badge: Int?,
    select: () -> Unit,
) {
    Card(
        colors = CardDefaults.cardColors(
            containerColor = if (selected) {
                MaterialTheme.colorScheme.primaryContainer
            } else {
                MaterialTheme.colorScheme.surfaceContainer
            },
        ),
        modifier = Modifier.fillMaxWidth()
            .selectable(selected = selected, enabled = option.available, role = Role.RadioButton) { select() }
            .testTag(tag),
    ) {
        Row(
            modifier = Modifier.padding(16.dp).heightIn(min = 56.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            RadioButton(selected = selected, onClick = null, enabled = option.available)
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(stringResource(option.title), style = MaterialTheme.typography.titleMedium)
                Text(
                    stringResource(option.body),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                badge?.let {
                    AssistChip(
                        onClick = select,
                        enabled = option.available,
                        label = { Text(stringResource(it)) },
                        colors = AssistChipDefaults.assistChipColors(
                            labelColor = MaterialTheme.colorScheme.primary,
                        ),
                    )
                }
            }
        }
    }
}
