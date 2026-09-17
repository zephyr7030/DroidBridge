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
import com.droidbridge.android.product.settings.AgentType
import com.droidbridge.android.ui.settings.BackButton

private data class AgentOption(
    val type: AgentType,
    @StringRes val title: Int,
    @StringRes val body: Int,
    val available: Boolean,
)

private val agentOptions = listOf(
    AgentOption(AgentType.LocalMcp, R.string.agent_local_mcp, R.string.agent_local_mcp_body, available = true),
    AgentOption(AgentType.ChatGpt, R.string.agent_chatgpt, R.string.agent_chatgpt_body, available = true),
    AgentOption(AgentType.Other, R.string.agent_other, R.string.agent_other_body, available = false),
)

/** First-launch choice of how AI agents reach DroidBridge; ChatGPT is preselected. */
@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun AgentChoiceRoute(
    selected: AgentType,
    select: (AgentType) -> Unit,
    continueSetup: () -> Unit,
    back: () -> Unit,
) {
    Scaffold(
        modifier = Modifier.testTag("route:AgentChoice"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.agent_choice_title)) },
                navigationIcon = { BackButton("route:AgentChoice", R.string.agent_choice_title, back) },
            )
        },
        bottomBar = {
            Button(
                onClick = continueSetup,
                enabled = agentOptions.first { it.type == selected }.available,
                modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp).heightIn(min = 56.dp)
                    .testTag("agent_choice:continue"),
            ) { Text(stringResource(R.string.action_continue)) }
        },
    ) { padding ->
        Column(
            modifier = Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.agent_choice_description),
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            agentOptions.forEach { option ->
                val isSelected = option.type == selected
                Card(
                    colors = CardDefaults.cardColors(
                        containerColor = if (isSelected) {
                            MaterialTheme.colorScheme.primaryContainer
                        } else {
                            MaterialTheme.colorScheme.surfaceContainer
                        },
                    ),
                    modifier = Modifier.fillMaxWidth()
                        .selectable(
                            selected = isSelected,
                            enabled = option.available,
                            role = Role.RadioButton,
                        ) { select(option.type) }
                        .testTag("agent_choice:${option.type.wireValue}"),
                ) {
                    Row(
                        modifier = Modifier.padding(16.dp).heightIn(min = 56.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(16.dp),
                    ) {
                        RadioButton(selected = isSelected, onClick = null, enabled = option.available)
                        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text(stringResource(option.title), style = MaterialTheme.typography.titleMedium)
                            Text(
                                stringResource(option.body),
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            }
        }
    }
}
