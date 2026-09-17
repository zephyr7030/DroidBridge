package com.droidbridge.android.ui

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.provider.Settings as AndroidSettings
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.DrawableRes
import androidx.annotation.StringRes
import androidx.compose.animation.ContentTransform
import androidx.compose.animation.EnterTransition
import androidx.compose.animation.ExitTransition
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.TextButton
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemColors
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.adaptive.ExperimentalMaterial3AdaptiveApi
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteScaffold
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.graphics.drawable.toBitmap
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavEntryDecorator
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay
import com.droidbridge.android.AppGraph
import com.droidbridge.android.BuildConfig
import com.droidbridge.android.R
import com.droidbridge.android.client.AvailabilityState
import com.droidbridge.android.client.BackgroundFacts
import com.droidbridge.android.client.BackgroundRows
import com.droidbridge.android.client.CapabilityAction
import com.droidbridge.android.client.CapabilityRow
import com.droidbridge.android.client.CapabilityRowKey
import com.droidbridge.android.client.CapabilityRowState
import com.droidbridge.android.client.CapabilityRows
import com.droidbridge.android.client.ClientState
import com.droidbridge.android.client.RuntimeReadiness
import com.droidbridge.android.product.home.HomeMcpRow
import com.droidbridge.android.ui.setup.DeviceSetup
import com.droidbridge.android.product.about.ProductInfo
import com.droidbridge.android.product.settings.AgentType
import com.droidbridge.android.product.settings.ThemePreference
import com.droidbridge.android.ui.automation.AutomationEditorRoute
import com.droidbridge.android.ui.automation.AutomationEditorViewModel
import com.droidbridge.android.ui.automation.AutomationListViewModel
import com.droidbridge.android.ui.automation.AutomationsRoute
import com.droidbridge.android.ui.common.ReasonText
import com.droidbridge.android.ui.diagnostics.DiagnosticsRoute
import com.droidbridge.android.ui.diagnostics.DiagnosticsViewModel
import com.droidbridge.android.ui.home.HomeDestination
import com.droidbridge.android.ui.home.HomeRoute
import com.droidbridge.android.ui.home.HomeViewModel
import com.droidbridge.android.ui.maintenance.MaintenanceRecoveryRoute
import com.droidbridge.android.ui.maintenance.MaintenanceViewModel
import com.droidbridge.android.ui.mcp.AgentConnectionRoute
import com.droidbridge.android.ui.mcp.McpRoute
import com.droidbridge.android.ui.mcp.McpViewModel
import com.droidbridge.android.ui.mcp.TunnelRoute
import com.droidbridge.android.ui.mcp.TunnelViewModel
import com.droidbridge.android.ui.settings.AboutRoute
import com.droidbridge.android.ui.settings.DataRoute
import com.droidbridge.android.ui.settings.DataViewModel
import com.droidbridge.android.ui.settings.LicensesRoute
import com.droidbridge.android.ui.settings.SettingsDestination
import com.droidbridge.android.ui.settings.SettingsRoute
import com.droidbridge.android.ui.state.AppUiState
import com.droidbridge.android.ui.state.AppViewModel
import com.droidbridge.android.ui.tasks.TaskDetailRoute
import com.droidbridge.android.ui.tasks.TaskDetailViewModel
import com.droidbridge.android.ui.tasks.TaskListViewModel
import com.droidbridge.android.ui.tasks.TasksRoute
import com.droidbridge.android.ui.theme.DroidBridgeTheme
import com.droidbridge.android.ui.onboarding.AgentChoiceRoute
import com.droidbridge.android.ui.updates.UpdatesRoute
import com.droidbridge.android.ui.updates.UpdatesViewModel
import kotlinx.serialization.Serializable

@Serializable data object Welcome : NavKey
@Serializable data object AgentChoice : NavKey
@Serializable data object Main : NavKey
@Serializable data object Home : NavKey
@Serializable data object Capabilities : NavKey
@Serializable data object Tasks : NavKey
@Serializable data class TaskDetail(val taskId: String) : NavKey
@Serializable data object Automations : NavKey
@Serializable data class AutomationEditor(val automationId: String? = null) : NavKey
@Serializable data object AgentConnections : NavKey
@Serializable data object MCP : NavKey
@Serializable data object TunnelSetup : NavKey
@Serializable data object Diagnostics : NavKey
@Serializable data object Settings : NavKey
@Serializable data object Updates : NavKey
@Serializable data object Data : NavKey
@Serializable data object About : NavKey
@Serializable data object Licenses : NavKey
@Serializable data object MaintenanceRecovery : NavKey

private data class PrimaryDestination(
    val key: NavKey,
    @StringRes val label: Int,
    @DrawableRes val icon: Int,
    val tag: String,
)

/**
 * The four swipeable tabs in their spatial order. Settings sits left of Home so every tab is at most
 * two swipes from the Home start page; the order also fixes each tab switch's slide direction.
 */
private val primaryDestinations = listOf(
    PrimaryDestination(Settings, R.string.nav_settings, R.drawable.ic_nav_settings, "nav:settings"),
    PrimaryDestination(Home, R.string.nav_home, R.drawable.ic_nav_home, "nav:home"),
    PrimaryDestination(Tasks, R.string.nav_tasks, R.drawable.ic_nav_tasks, "nav:tasks"),
    PrimaryDestination(Automations, R.string.nav_automations, R.drawable.ic_nav_automations, "nav:automations"),
)

private const val SETTINGS_TAB = 0
private const val HOME_TAB = 1
private const val TASKS_TAB = 2
private const val AUTOMATIONS_TAB = 3

private const val PAGE_SLIDE_MILLIS = 300

/** Pages keep a readable line length on wide windows such as a landscape phone; the page ground fills the rest. */
private val READABLE_PAGE_WIDTH = 720.dp

/** The primary shell spans the window so its navigation rail stays at the edge; it bounds each tab page itself. */
private const val FULL_WIDTH_ENTRY = "droidbridge.full_width"

private val ReadableWidthDecorator = NavEntryDecorator<NavKey> { entry ->
    if (entry.metadata[FULL_WIDTH_ENTRY] == true) entry.Content() else ReadableWidth { entry.Content() }
}

@Composable
private fun ReadableWidth(content: @Composable () -> Unit) {
    Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background), contentAlignment = Alignment.TopCenter) {
        Box(Modifier.widthIn(max = READABLE_PAGE_WIDTH).fillMaxHeight()) { content() }
    }
}

/** A child page slides in from the right over the page that stays beneath it. */
private val PushTransition = ContentTransform(
    targetContentEnter = slideInHorizontally(tween(PAGE_SLIDE_MILLIS)) { width -> width },
    // Fully opaque for the whole slide, so the parent stays visible beneath the incoming page.
    initialContentExit = fadeOut(tween(PAGE_SLIDE_MILLIS), targetAlpha = 1f),
    targetContentZIndex = 1f,
)

/** Going back slides the child page out to the right, uncovering the page beneath. */
private val PopTransition = ContentTransform(
    targetContentEnter = EnterTransition.None,
    initialContentExit = slideOutHorizontally(tween(PAGE_SLIDE_MILLIS)) { width -> width },
    targetContentZIndex = -1f,
)

private val settledCapabilityStates = setOf(
    CapabilityRowState.Ready,
    CapabilityRowState.Connected,
    CapabilityRowState.Active,
    CapabilityRowState.KeptByModule,
    CapabilityRowState.KeptByShizuku,
    CapabilityRowState.Confirmed,
)

/** Device facts read outside the Runtime: they change in system settings, so they are reread on every resume. */
private data class DeviceSetupState(val background: BackgroundFacts, val rootDetected: Boolean, val moduleAbsent: Boolean)

@Composable
private fun rememberDeviceSetup(state: AppUiState): DeviceSetupState {
    val context = LocalContext.current
    val confirmations = state.backgroundConfirmations
    fun read() = DeviceSetup.backgroundFacts(context, confirmations.autostart, confirmations.recentsLock) to
        DeviceSetup.rootDetected(context)
    var facts by remember(confirmations) { mutableStateOf(read()) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { facts = read() }
    return DeviceSetupState(facts.first, facts.second, state.moduleAbsent)
}

@Composable
fun DroidBridgeUi(viewModel: AppViewModel, graph: AppGraph) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LaunchedEffect(state.onboardingCompleted) {
        if (state.onboardingCompleted == true) viewModel.startRuntime()
    }
    val darkTheme = when (state.theme) {
        ThemePreference.System -> androidx.compose.foundation.isSystemInDarkTheme()
        ThemePreference.Light -> false
        ThemePreference.Dark -> true
    }
    DroidBridgeTheme(darkTheme) {
        when (state.onboardingCompleted) {
            null -> LoadingRoute(R.string.app_name, "route:Welcome")
            else -> NavigationRoot(state, viewModel, graph)
        }
    }
}

@Composable
private fun LoadingRoute(@StringRes title: Int, tag: String) {
    val loadingDescription = stringResource(R.string.state_loading)
    RouteFrame(title, tag) {
        Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator(
                modifier = Modifier.semantics { contentDescription = loadingDescription },
            )
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3AdaptiveApi::class)
private fun NavigationRoot(state: AppUiState, viewModel: AppViewModel, graph: AppGraph) {
    val initial = if (state.onboardingCompleted == true) Main else Welcome
    val backStack = rememberNavBackStack(initial)
    var selectedTab by rememberSaveable { mutableIntStateOf(HOME_TAB) }
    val navigate: (NavKey) -> Unit = { destination ->
        val tab = primaryDestinations.indexOfFirst { it.key == destination }
        if (tab < 0) {
            backStack.add(destination)
        } else {
            selectedTab = tab
            if (backStack.size != 1 || backStack.first() != Main) {
                backStack.clear()
                backStack.add(Main)
            }
        }
    }
    // MaintenanceRecovery is the bootstrap root exactly while a maintenance blocker exists.
    LaunchedEffect(state.maintenance?.recoveryRequired) {
        if (state.maintenance?.recoveryRequired == true && backStack.lastOrNull() != MaintenanceRecovery) {
            backStack.clear()
            backStack.add(MaintenanceRecovery)
        }
    }
    NavDisplay(
        backStack = backStack,
        onBack = { backStack.removeLastOrNull() },
        // Each route entry owns its screen ViewModel, so a reopened editor requeries its owner.
        entryDecorators = listOf(
            rememberSaveableStateHolderNavEntryDecorator(),
            rememberViewModelStoreNavEntryDecorator(),
            ReadableWidthDecorator,
        ),
        transitionSpec = { PushTransition },
        popTransitionSpec = { PopTransition },
        predictivePopTransitionSpec = { PopTransition },
        entryProvider = entryProvider {
            entry<Welcome> { WelcomeScreen { navigate(AgentChoice) } }
            entry<AgentChoice> {
                AgentChoiceRoute(
                    selected = state.agentType,
                    select = viewModel::setAgentType,
                    continueSetup = { navigate(Capabilities) },
                ) { backStack.removeLastOrNull() }
            }
            entry<Main>(metadata = mapOf(FULL_WIDTH_ENTRY to true)) {
                val home = viewModel { HomeViewModel(graph.client, graph.tasks) }
                val homeState by home.state.collectAsStateWithLifecycle()
                val updateState by graph.updates.state.collectAsStateWithLifecycle()
                val available = state.clientState is ClientState.Available
                LaunchedEffect(selectedTab, available) { home.refresh() }
                LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { viewModel.recheckRuntime() }
                val capabilityAction = rememberCapabilityActionHandler(viewModel, navigate)
                val setup = rememberDeviceSetup(state)
                val snapshot = (state.clientState as? ClientState.Available)?.snapshot
                val connectionEnabled = homeState.tunnel?.enabled == true ||
                    homeState.projection?.mcp?.let { it != HomeMcpRow.Off } == true
                val attention = snapshot?.let { CapabilityRows.project(it, setup.rootDetected, setup.moduleAbsent) }.orEmpty()
                    .filter { it.action != null && it.state !in settledCapabilityStates } +
                    BackgroundRows.attention(setup.background, BackgroundRows.keeper(snapshot), connectionEnabled)
                PrimaryShell(
                    selected = selectedTab,
                    select = { selectedTab = it },
                    backToHome = backStack.size == 1,
                    taskBadge = homeState.projection?.takeIf { it.activeTaskCount > 0 }?.activeTasks,
                ) { page ->
                    when (page) {
                        HOME_TAB -> HomeRoute(
                            viewModel = home,
                            clientState = state.clientState,
                            attention = attention,
                            onCapabilityAction = { row -> row.action?.let { capabilityAction(row.key, it) } },
                            newerVersionAvailable = updateState.newerVersionAvailable,
                        ) { destination ->
                            backStack.add(
                                when (destination) {
                                    HomeDestination.Diagnostics -> Diagnostics
                                    HomeDestination.Capabilities -> Capabilities
                                    HomeDestination.AgentConnections -> AgentConnections
                                    HomeDestination.Updates -> Updates
                                },
                            )
                        }
                        TASKS_TAB -> TasksRoute(viewModel { TaskListViewModel(graph.tasks) }) { taskId ->
                            backStack.add(TaskDetail(taskId))
                        }
                        AUTOMATIONS_TAB -> AutomationsRoute(
                            viewModel = viewModel { AutomationListViewModel(graph.automations) },
                            openEditor = { id -> backStack.add(AutomationEditor(id)) },
                            openTask = { taskId -> backStack.add(TaskDetail(taskId)) },
                        )
                        SETTINGS_TAB -> SettingsRoute(state.theme, viewModel::setTheme) { destination ->
                            backStack.add(
                                when (destination) {
                                    SettingsDestination.Capabilities -> Capabilities
                                    SettingsDestination.AgentConnections -> AgentConnections
                                    SettingsDestination.Diagnostics -> Diagnostics
                                    SettingsDestination.Updates -> Updates
                                    SettingsDestination.Data -> Data
                                    SettingsDestination.About -> About
                                    SettingsDestination.Welcome -> Welcome
                                },
                            )
                        }
                    }
                }
            }
            entry<Capabilities> {
                CapabilitiesScreen(state, viewModel, {
                    // First setup ends where the chosen agent's ingress is configured; later visits just return.
                    val chosen = state.agentType.takeIf { state.onboardingCompleted != true }
                    viewModel.completeOnboarding()
                    when (chosen) {
                        AgentType.ChatGpt -> {
                            backStack.clear()
                            backStack.add(Main)
                            backStack.add(TunnelSetup)
                        }
                        AgentType.LocalMcp -> {
                            backStack.clear()
                            backStack.add(Main)
                            backStack.add(MCP)
                        }
                        else -> navigate(Home)
                    }
                }, { backStack.removeLastOrNull() }, navigate)
            }
            entry<TaskDetail> { key ->
                TaskDetailRoute(viewModel { TaskDetailViewModel(graph.tasks, key.taskId) }) { backStack.removeLastOrNull() }
            }
            entry<AutomationEditor> { key ->
                AutomationEditorRoute(
                    viewModel = viewModel {
                        AutomationEditorViewModel(graph.automations, graph.automationDescriptors, key.automationId)
                    },
                    catalog = graph.automationDescriptors,
                ) { backStack.removeLastOrNull() }
            }
            entry<AgentConnections> {
                AgentConnectionRoute(
                    viewModel = viewModel { McpViewModel(graph.client) },
                    openMcp = { backStack.add(MCP) },
                    openTunnel = { backStack.add(TunnelSetup) },
                ) { backStack.removeLastOrNull() }
            }
            entry<MCP> {
                val context = LocalContext.current
                McpRoute(
                    viewModel = viewModel { McpViewModel(graph.client) },
                    notificationsUnavailable = (state.clientState as? ClientState.Available)
                        ?.snapshot?.grants?.get("android.notifications")?.state == AvailabilityState.Unavailable,
                    shouldRequestNotifications = { shouldRequestPostNotifications(context) },
                ) { backStack.removeLastOrNull() }
            }
            entry<TunnelSetup> {
                val context = LocalContext.current
                TunnelRoute(
                    viewModel = viewModel { TunnelViewModel(graph.client) },
                    runtimeReady = (state.clientState as? ClientState.Available)
                        ?.snapshot?.readiness == RuntimeReadiness.Ready,
                    notificationsUnavailable = (state.clientState as? ClientState.Available)
                        ?.snapshot?.grants?.get("android.notifications")?.state == AvailabilityState.Unavailable,
                    shouldRequestNotifications = { shouldRequestPostNotifications(context) },
                    done = {
                        backStack.clear()
                        backStack.add(Main)
                    },
                ) { backStack.removeLastOrNull() }
            }
            entry<Diagnostics> {
                DiagnosticsRoute(
                    viewModel = viewModel { DiagnosticsViewModel(graph.diagnosticsExporter) },
                    apkVersion = graph.apkVersionName,
                ) { backStack.removeLastOrNull() }
            }
            entry<Updates> {
                UpdatesRoute(
                    viewModel = viewModel { UpdatesViewModel(graph.client, graph.updates, graph.updateCache) },
                    apkVersion = graph.apkVersionName,
                ) { backStack.removeLastOrNull() }
            }
            entry<Data> {
                DataRoute(viewModel { DataViewModel(graph.client, graph.updateCache) }) { backStack.removeLastOrNull() }
            }
            entry<About> {
                AboutRoute(
                    repositoryUrl = ProductInfo.repositoryUrl(BuildConfig.GITHUB_OWNER, BuildConfig.GITHUB_REPO),
                    openLicenses = { backStack.add(Licenses) },
                ) { backStack.removeLastOrNull() }
            }
            entry<Licenses> {
                LicensesRoute(graph.licenses, notices = { graph.thirdPartyNotices }) { backStack.removeLastOrNull() }
            }
            entry<MaintenanceRecovery> {
                MaintenanceRecoveryRoute(viewModel { MaintenanceViewModel(graph.client, graph.diagnosticsExporter) }) {
                    // Successful recovery re-evaluates onboarding/Main instead of creating a second stack.
                    viewModel.refreshMaintenance()
                    backStack.clear()
                    backStack.add(if (state.onboardingCompleted == true) Main else Welcome)
                }
            }
        },
    )
}

/**
 * The tab bar and the horizontal pager are two views of one selection: a tap slides the pager to the
 * tab in its spatial direction, and a settled swipe selects its tab. Back from another tab returns to
 * Home first.
 */
@Composable
private fun PrimaryShell(
    selected: Int,
    select: (Int) -> Unit,
    backToHome: Boolean,
    taskBadge: String?,
    page: @Composable (Int) -> Unit,
) {
    val pager = rememberPagerState(initialPage = selected) { primaryDestinations.size }
    LaunchedEffect(selected) { if (pager.currentPage != selected) pager.animateScrollToPage(selected) }
    // A settle between a cancelled tab animation and its replacement must not overwrite the new target.
    LaunchedEffect(pager) { snapshotFlow { pager.settledPage }.collect { if (!pager.isScrollInProgress) select(it) } }
    BackHandler(enabled = backToHome && selected != HOME_TAB) { select(HOME_TAB) }
    NavigationSuiteScaffold(
        navigationSuiteItems = {
            primaryDestinations.forEachIndexed { index, destination ->
                item(
                    selected = pager.currentPage == index,
                    onClick = { select(index) },
                    icon = {
                        BadgedBox(
                            badge = {
                                if (index == TASKS_TAB && taskBadge != null) {
                                    Badge(modifier = Modifier.testTag("nav:tasks:badge")) { Text(taskBadge) }
                                }
                            },
                        ) {
                            Icon(
                                painterResource(destination.icon),
                                contentDescription = stringResource(destination.label),
                                modifier = Modifier.testTag(destination.tag),
                            )
                        }
                    },
                    label = { Text(stringResource(destination.label)) },
                )
            }
        },
    ) {
        HorizontalPager(
            state = pager,
            key = { primaryDestinations[it].tag },
            modifier = Modifier.fillMaxSize(),
        ) { index -> ReadableWidth { page(index) } }
    }
}

@Composable
private fun WelcomeScreen(start: () -> Unit) {
    val context = LocalContext.current
    val icon = remember { context.packageManager.getApplicationIcon(context.packageName).toBitmap().asImageBitmap() }
    Scaffold(
        modifier = Modifier.testTag("route:Welcome"),
        bottomBar = {
            Button(
                onClick = start,
                modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp).heightIn(min = 56.dp)
                    .testTag("welcome:start_setup"),
            ) { Text(stringResource(R.string.welcome_start_setup)) }
        },
    ) { padding ->
        Column(
            modifier = Modifier.fillMaxSize().padding(padding).padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Image(icon, contentDescription = null, modifier = Modifier.size(96.dp))
            Text(stringResource(R.string.app_name), style = MaterialTheme.typography.headlineLarge)
            Text(
                stringResource(R.string.welcome_description),
                style = MaterialTheme.typography.bodyLarge,
                textAlign = TextAlign.Center,
            )
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun CapabilitiesScreen(
    state: AppUiState,
    viewModel: AppViewModel,
    onEnter: () -> Unit,
    onBack: () -> Unit,
    navigate: (NavKey) -> Unit,
) {
    LaunchedEffect(Unit) { viewModel.startRuntime() }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { viewModel.recheckRuntime() }
    val available = state.clientState as? ClientState.Available
    val snapshot = available?.snapshot
    val unavailableReason = (state.clientState as? ClientState.Unavailable)?.reason
    val setup = rememberDeviceSetup(state)
    val rows = snapshot?.let { CapabilityRows.project(it, setup.rootDetected, setup.moduleAbsent) } ?: listOf(
        CapabilityRow(
            CapabilityRowKey.Runtime,
            if (state.clientState is ClientState.Unavailable) CapabilityRowState.Unavailable else CapabilityRowState.Starting,
            if (state.clientState is ClientState.Unavailable) {
                if (unavailableReason in setOf("CLEANUP_UNVERIFIED", "PROTOCOL_MISMATCH", "MODULE_CONFLICT")) {
                    CapabilityAction.Diagnostics
                } else {
                    CapabilityAction.Retry
                }
            } else {
                null
            },
            unavailableReason,
        ),
        CapabilityRow(CapabilityRowKey.RootBackend, CapabilityRowState.Starting),
        CapabilityRow(CapabilityRowKey.Shizuku, CapabilityRowState.Connecting),
    )
    val actionHandler = rememberCapabilityActionHandler(viewModel, navigate)
    Scaffold(
        modifier = Modifier.testTag("route:Capabilities"),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.capabilities_title)) },
                navigationIcon = {
                    IconButton(onClick = onBack, modifier = Modifier.testTag("route:Capabilities:back")) {
                        Icon(
                            painterResource(R.drawable.ic_arrow_back),
                            contentDescription = stringResource(R.string.capabilities_title),
                        )
                    }
                },
            )
        },
        bottomBar = {
            Button(
                onClick = onEnter,
                enabled = snapshot?.readiness == RuntimeReadiness.Ready,
                modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp).heightIn(min = 56.dp)
                    .testTag("capabilities:enter"),
            ) { Text(stringResource(R.string.capabilities_enter)) }
        },
    ) { padding ->
        val background = BackgroundRows.project(setup.background, BackgroundRows.keeper(snapshot))
        val pending = (rows + background).filter { it.action != null && it.state !in settledCapabilityStates }
        // The next step is the first open item; the others stay visible but quieter.
        val next = pending.firstOrNull()?.key
        LazyColumn(modifier = Modifier.fillMaxSize().padding(padding)) {
            item(key = "capabilities:summary") {
                Text(
                    if (pending.isEmpty()) {
                        stringResource(R.string.capabilities_all_set)
                    } else {
                        pluralStringResource(R.plurals.capabilities_remaining, pending.size, pending.size)
                    },
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp).testTag("capabilities:summary"),
                )
            }
            item(key = "capabilities:section:access") { CapabilitySection(R.string.capabilities_section_access) }
            itemsIndexed(rows, key = { _, row -> row.key }) { index, row ->
                CapabilityListItem(
                    row = row,
                    refreshing = row.key == CapabilityRowKey.Runtime && available?.refreshing == true,
                    emphasized = row.key == next,
                ) { row.action?.let { actionHandler(row.key, it) } }
                if (index != rows.lastIndex) HorizontalDivider()
            }
            item(key = "capabilities:section:background") { CapabilitySection(R.string.capabilities_section_background) }
            itemsIndexed(background, key = { _, row -> row.key }) { index, row ->
                CapabilityListItem(row = row, refreshing = false, emphasized = row.key == next) {
                    row.action?.let { actionHandler(row.key, it) }
                }
                if (index != background.lastIndex) HorizontalDivider()
            }
        }
    }
}

@Composable
internal fun CapabilityListItem(
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

@Composable
private fun CapabilitySection(@StringRes title: Int) {
    Text(
        stringResource(title),
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

private enum class SetupDialog { RestrictedSettings, AutostartConfirm, RecentsLock }

@Composable
private fun rememberCapabilityActionHandler(
    viewModel: AppViewModel,
    navigate: (NavKey) -> Unit,
): (CapabilityRowKey, CapabilityAction) -> Unit {
    val context = LocalContext.current
    val captureManager = context.getSystemService(MediaProjectionManager::class.java)
    val capture = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (result.resultCode == Activity.RESULT_OK && result.data != null) {
            viewModel.deliverMediaProjectionConsent(result.resultCode, result.data!!)
        }
        viewModel.recheckRuntime()
    }
    val notifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        capture.launch(captureManager.createScreenCaptureIntent())
    }
    val localNetwork = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { viewModel.recheckRuntime() }
    var dialog by remember { mutableStateOf<SetupDialog?>(null) }
    // Android cannot report the vendor autostart switch, so returning from that page asks the user.
    var awaitingAutostart by remember { mutableStateOf(false) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) {
        if (awaitingAutostart) {
            awaitingAutostart = false
            dialog = SetupDialog.AutostartConfirm
        }
    }
    when (dialog) {
        SetupDialog.RestrictedSettings -> AlertDialog(
            onDismissRequest = { dialog = null },
            title = { Text(stringResource(R.string.restricted_settings_title)) },
            text = { Text(stringResource(R.string.restricted_settings_body)) },
            confirmButton = {
                TextButton(
                    onClick = { dialog = null; DeviceSetup.openAccessibility(context) },
                    modifier = Modifier.testTag("setup:restricted:continue"),
                ) { Text(stringResource(R.string.action_continue)) }
            },
            dismissButton = {
                TextButton(
                    onClick = { dialog = null; DeviceSetup.openAppDetails(context) },
                    modifier = Modifier.testTag("setup:restricted:app_details"),
                ) { Text(stringResource(R.string.action_open_app_details)) }
            },
        )
        SetupDialog.AutostartConfirm -> AlertDialog(
            onDismissRequest = { dialog = null },
            title = { Text(stringResource(R.string.autostart_confirm_title)) },
            text = { Text(stringResource(R.string.autostart_confirm_body)) },
            confirmButton = {
                TextButton(
                    onClick = { dialog = null; viewModel.confirmAutostart() },
                    modifier = Modifier.testTag("setup:autostart:confirm"),
                ) { Text(stringResource(R.string.action_confirm_allowed)) }
            },
            dismissButton = { TextButton(onClick = { dialog = null }) { Text(stringResource(R.string.action_not_yet)) } },
        )
        SetupDialog.RecentsLock -> AlertDialog(
            onDismissRequest = { dialog = null },
            title = { Text(stringResource(R.string.recents_lock_title)) },
            text = { Text(stringResource(R.string.recents_lock_body)) },
            confirmButton = {
                TextButton(
                    onClick = { dialog = null; viewModel.confirmRecentsLock() },
                    modifier = Modifier.testTag("setup:recents:confirm"),
                ) { Text(stringResource(R.string.action_confirm_locked)) }
            },
            dismissButton = { TextButton(onClick = { dialog = null }) { Text(stringResource(R.string.action_cancel)) } },
        )
        null -> Unit
    }
    return { row, action ->
        when (action) {
            CapabilityAction.Retry, CapabilityAction.Recheck -> viewModel.recheckRuntime()
            CapabilityAction.Authorize -> viewModel.requestShizukuAuthorization()
            CapabilityAction.Diagnostics -> navigate(Diagnostics)
            CapabilityAction.InstallModule -> DeviceSetup.openModuleDownload(context)
            CapabilityAction.UpdateModule -> navigate(Updates)
            CapabilityAction.InstallShizuku -> context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://shizuku.rikka.app/download/")))
            CapabilityAction.OpenShizuku -> context.packageManager.getLaunchIntentForPackage("moe.shizuku.privileged.api")?.let(context::startActivity)
            CapabilityAction.Allow -> {
                if (row == CapabilityRowKey.LocalNetwork) {
                    if (Build.VERSION.SDK_INT >= 37) localNetwork.launch(localNetworkPermission())
                } else {
                    context.startActivity(Intent(AndroidSettings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM, Uri.parse("package:${context.packageName}")))
                }
            }
            CapabilityAction.OpenSettings -> when {
                row != CapabilityRowKey.Accessibility -> DeviceSetup.openNotificationAccess(context)
                DeviceSetup.restrictedSettingsApply(context) -> dialog = SetupDialog.RestrictedSettings
                else -> DeviceSetup.openAccessibility(context)
            }
            CapabilityAction.AllowBattery -> DeviceSetup.requestBatteryExemption(context)
            CapabilityAction.OpenAppDetails -> DeviceSetup.openAppDetails(context)
            CapabilityAction.OpenAutostart -> {
                awaitingAutostart = true
                DeviceSetup.openVendorAutostart(context)
            }
            CapabilityAction.ShowRecentsLockHelp -> dialog = SetupDialog.RecentsLock
            CapabilityAction.StartCapture -> {
                if (shouldRequestPostNotifications(context)) {
                    notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                } else capture.launch(captureManager.createScreenCaptureIntent())
            }
            CapabilityAction.StopCapture -> viewModel.stopMediaProjection()
        }
    }
}

internal fun shouldRequestPostNotifications(context: Context): Boolean {
    if (context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == android.content.pm.PackageManager.PERMISSION_GRANTED) {
        return false
    }
    return context is Activity && !context.shouldShowRequestPermissionRationale(Manifest.permission.POST_NOTIFICATIONS)
}

@androidx.annotation.RequiresApi(37)
private fun localNetworkPermission(): String = Manifest.permission.ACCESS_LOCAL_NETWORK

@Composable
@OptIn(ExperimentalMaterial3Api::class)
private fun RouteFrame(@StringRes title: Int, tag: String, back: (() -> Unit)? = null, content: @Composable () -> Unit) {
    Scaffold(
        modifier = Modifier.testTag(tag),
        topBar = {
            TopAppBar(
                title = { Text(stringResource(title)) },
                navigationIcon = if (back == null) ({}) else ({
                    IconButton(onClick = back, modifier = Modifier.testTag("$tag:back")) {
                        Icon(painterResource(R.drawable.ic_arrow_back), stringResource(title))
                    }
                }),
            )
        },
    ) { padding -> Column(Modifier.fillMaxSize().padding(padding)) { content() } }
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
    CapabilityRowState.Unavailable ->
        if (row.reason == CapabilityRows.ROOT_NOT_DETECTED) R.string.state_root_not_detected else R.string.state_unavailable
    CapabilityRowState.NotInstalled -> when (row.key) {
        CapabilityRowKey.Shizuku -> R.string.shizuku_state_not_installed
        CapabilityRowKey.RootBackend -> R.string.state_root_without_module
        else -> R.string.state_not_installed
    }
    CapabilityRowState.UpdateRequired -> R.string.state_update_required
    CapabilityRowState.Conflict -> R.string.state_conflict
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
    CapabilityRowState.NotConfirmed -> R.string.state_not_confirmed
    CapabilityRowState.Confirmed -> R.string.state_confirmed
}

@StringRes private fun actionText(action: CapabilityAction): Int = when (action) {
    CapabilityAction.Retry -> R.string.action_retry
    CapabilityAction.Recheck -> R.string.action_recheck
    CapabilityAction.Allow -> R.string.action_allow
    CapabilityAction.Diagnostics -> R.string.diagnostics_title
    CapabilityAction.InstallModule -> R.string.action_install_module
    CapabilityAction.UpdateModule -> R.string.action_update_module
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
}

@StringRes private fun rowReason(row: CapabilityRow): Int? {
    if (row.key != CapabilityRowKey.Runtime) return null
    return row.reason?.let(ReasonText::resource)
}

@DrawableRes private fun statusIcon(state: CapabilityRowState): Int = when (state) {
    CapabilityRowState.Ready, CapabilityRowState.Connected, CapabilityRowState.Active,
    CapabilityRowState.KeptByModule, CapabilityRowState.KeptByShizuku, CapabilityRowState.Confirmed -> R.drawable.ic_status_success
    CapabilityRowState.NotConfirmed -> R.drawable.ic_status_unknown
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
