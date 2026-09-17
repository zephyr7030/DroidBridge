package com.droidbridge.android.product.settings

import android.content.Context
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

private val Context.uiPreferences by preferencesDataStore(name = "ui_preferences")

enum class ThemePreference(val wireValue: String) {
    System("system"),
    Light("light"),
    Dark("dark"),
}

/** How the user intends AI agents to reach DroidBridge; chosen on first launch, ChatGPT by default. */
enum class AgentType(val wireValue: String) {
    LocalMcp("local_mcp"),
    ChatGpt("chatgpt"),
    Other("other"),
}

data class BackgroundConfirmations(val autostart: Boolean = false, val recentsLock: Boolean = false)

class AppSettings(context: Context) {
    private val dataStore = context.applicationContext.uiPreferences

    val onboardingCompleted: Flow<Boolean> = dataStore.data.map { preferences ->
        preferences[ONBOARDING_COMPLETED] ?: false
    }

    val theme: Flow<ThemePreference> = dataStore.data.map { preferences ->
        ThemePreference.entries.firstOrNull { it.wireValue == preferences[THEME] }
            ?: ThemePreference.System
    }

    val agentType: Flow<AgentType> = dataStore.data.map { preferences ->
        AgentType.entries.firstOrNull { it.wireValue == preferences[AGENT_TYPE] } ?: AgentType.ChatGpt
    }

    /** The user's own word for the two background settings Android cannot report back. */
    val backgroundConfirmations: Flow<BackgroundConfirmations> = dataStore.data.map { preferences ->
        BackgroundConfirmations(
            autostart = preferences[AUTOSTART_CONFIRMED] ?: false,
            recentsLock = preferences[RECENTS_LOCK_CONFIRMED] ?: false,
        )
    }

    suspend fun confirmAutostart() {
        dataStore.edit { preferences -> preferences[AUTOSTART_CONFIRMED] = true }
    }

    suspend fun confirmRecentsLock() {
        dataStore.edit { preferences -> preferences[RECENTS_LOCK_CONFIRMED] = true }
    }

    suspend fun completeOnboarding() {
        dataStore.edit { preferences -> preferences[ONBOARDING_COMPLETED] = true }
    }

    suspend fun setTheme(theme: ThemePreference) {
        dataStore.edit { preferences -> preferences[THEME] = theme.wireValue }
    }

    suspend fun setAgentType(agentType: AgentType) {
        dataStore.edit { preferences -> preferences[AGENT_TYPE] = agentType.wireValue }
    }

    companion object {
        private val ONBOARDING_COMPLETED = booleanPreferencesKey("onboarding_completed")
        private val THEME = stringPreferencesKey("theme")
        private val AGENT_TYPE = stringPreferencesKey("agent_type")
        private val AUTOSTART_CONFIRMED = booleanPreferencesKey("autostart_confirmed")
        private val RECENTS_LOCK_CONFIRMED = booleanPreferencesKey("recents_lock_confirmed")
    }
}
