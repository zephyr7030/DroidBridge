package com.droidbridge.android.product.settings

import android.content.Context
import com.droidbridge.android.client.SetupRoute
import androidx.datastore.core.handlers.ReplaceFileCorruptionHandler
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.emptyPreferences
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import java.io.IOException
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.map

// A file this App cannot read is replaced by its defaults: the first screen waits on these
// preferences, so a read that ends the flow would leave the App loading forever, every launch.
private val Context.uiPreferences by preferencesDataStore(
    name = "ui_preferences",
    corruptionHandler = ReplaceFileCorruptionHandler { emptyPreferences() },
)

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
    private val preferences: Flow<Preferences> = dataStore.data.catch { failure ->
        if (failure is IOException) emit(emptyPreferences()) else throw failure
    }

    val onboardingCompleted: Flow<Boolean> = preferences.map { preferences ->
        preferences[ONBOARDING_COMPLETED] ?: false
    }

    val theme: Flow<ThemePreference> = preferences.map { preferences ->
        ThemePreference.entries.firstOrNull { it.wireValue == preferences[THEME] }
            ?: ThemePreference.System
    }

    val agentType: Flow<AgentType> = preferences.map { preferences ->
        AgentType.entries.firstOrNull { it.wireValue == preferences[AGENT_TYPE] } ?: AgentType.ChatGpt
    }

    /** Null until first setup chooses one; the guide then asks only for that route's steps. */
    val setupRoute: Flow<SetupRoute?> = preferences.map { preferences ->
        SetupRoute.entries.firstOrNull { it.wireValue == preferences[SETUP_ROUTE] }
    }

    /** The user's own word for the two background settings Android cannot report back. */
    val backgroundConfirmations: Flow<BackgroundConfirmations> = preferences.map { preferences ->
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

    suspend fun setSetupRoute(route: SetupRoute) {
        dataStore.edit { preferences -> preferences[SETUP_ROUTE] = route.wireValue }
    }

    companion object {
        private val ONBOARDING_COMPLETED = booleanPreferencesKey("onboarding_completed")
        private val THEME = stringPreferencesKey("theme")
        private val AGENT_TYPE = stringPreferencesKey("agent_type")
        private val SETUP_ROUTE = stringPreferencesKey("setup_route")
        private val AUTOSTART_CONFIRMED = booleanPreferencesKey("autostart_confirmed")
        private val RECENTS_LOCK_CONFIRMED = booleanPreferencesKey("recents_lock_confirmed")
    }
}
