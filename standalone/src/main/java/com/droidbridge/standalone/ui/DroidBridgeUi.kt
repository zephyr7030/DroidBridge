package com.droidbridge.standalone.ui

import com.droidbridge.ui.home.agentSummary
import com.droidbridge.ui.settings.SettingsStatus
import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.provider.Settings as AndroidSettings
import androidx.activity.compose.BackHandler
import androidx.activity.compose.LocalActivity
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
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.launch
import com.droidbridge.standalone.execution.shizuku.ShizukuManagers
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
import com.droidbridge.standalone.AppGraph
import com.droidbridge.standalone.DroidBridgeApplication
import com.droidbridge.standalone.BuildConfig
import com.droidbridge.ui.R
import com.droidbridge.standalone.R as AppR
import com.droidbridge.ui.common.CapabilityListItem
import com.droidbridge.ui.client.AvailabilityState
import com.droidbridge.standalone.client.BackgroundFacts
import com.droidbridge.standalone.client.BackgroundRows
import com.droidbridge.ui.client.CapabilityAction
import com.droidbridge.ui.client.CapabilityRow
import com.droidbridge.ui.client.CapabilityRowKey
import com.droidbridge.ui.client.CapabilityRowState
import com.droidbridge.standalone.client.CapabilityRows
import com.droidbridge.standalone.client.SetupSteps
import com.droidbridge.ui.client.settledCapabilityStates
import com.droidbridge.ui.client.ClientState
import com.droidbridge.ui.client.RuntimeReadiness
import com.droidbridge.ui.product.home.HomeMcpRow
import com.droidbridge.standalone.ui.setup.DeviceSetup
import com.droidbridge.ui.product.about.ProductInfo
import com.droidbridge.ui.product.settings.AgentType
import com.droidbridge.ui.product.settings.ThemePreference
import com.droidbridge.ui.automation.AutomationDetailRoute
import com.droidbridge.ui.automation.AutomationDetailViewModel
import com.droidbridge.ui.automation.AutomationEditorRoute
import com.droidbridge.ui.automation.AutomationEditorViewModel
import com.droidbridge.ui.automation.AutomationListViewModel
import com.droidbridge.ui.automation.AutomationsRoute
import com.droidbridge.ui.common.ReasonText
import com.droidbridge.ui.diagnostics.DiagnosticsRoute
import com.droidbridge.ui.diagnostics.DiagnosticsViewModel
import com.droidbridge.ui.home.HomeDestination
import com.droidbridge.ui.home.HomeRoute
import com.droidbridge.ui.home.HomeViewModel
import com.droidbridge.ui.maintenance.MaintenanceRecoveryRoute
import com.droidbridge.ui.maintenance.MaintenanceViewModel
import com.droidbridge.ui.mcp.AgentConnectionRoute
import com.droidbridge.ui.mcp.McpRoute
import com.droidbridge.ui.mcp.McpViewModel
import com.droidbridge.ui.mcp.TunnelRoute
import com.droidbridge.ui.mcp.TunnelViewModel
import com.droidbridge.ui.settings.AboutRoute
import com.droidbridge.ui.settings.DataRoute
import com.droidbridge.ui.settings.DataViewModel
import com.droidbridge.ui.settings.LicensesRoute
import com.droidbridge.ui.settings.SettingsDestination
import com.droidbridge.ui.settings.SettingsRoute
import com.droidbridge.standalone.ui.state.AppUiState
import com.droidbridge.standalone.ui.state.AppViewModel
import com.droidbridge.ui.tasks.TaskDetailRoute
import com.droidbridge.ui.tasks.TaskDetailViewModel
import com.droidbridge.ui.theme.DroidBridgeTheme
import com.droidbridge.standalone.ui.onboarding.SetupChoiceFacts
import com.droidbridge.standalone.ui.onboarding.SetupChoiceRoute
import com.droidbridge.standalone.ui.updates.UpdatesRoute
import com.droidbridge.standalone.ui.updates.UpdatesViewModel
import kotlinx.serialization.Serializable

@Serializable data object Welcome : NavKey
@Serializable data object SetupChoice : NavKey
@Serializable data object Main : NavKey
@Serializable data object Home : NavKey
@Serializable data object Capabilities : NavKey
@Serializable data class TaskDetail(val taskId: String) : NavKey
@Serializable data object Automations : NavKey
@Serializable data class AutomationDetail(val automationId: String) : NavKey
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
 * The swipeable tabs in their spatial order, Home in the middle one swipe from each; the order also
 * fixes each tab switch's slide direction. Tasks live on Home, running work first.
 */
private val primaryDestinations = listOf(
    PrimaryDestination(Settings, R.string.nav_settings, R.drawable.ic_nav_settings, "nav:settings"),
    PrimaryDestination(Home, R.string.nav_home, R.drawable.ic_nav_home, "nav:home"),
    PrimaryDestination(Automations, R.string.nav_automations, R.drawable.ic_nav_automations, "nav:automations"),
)

private const val SETTINGS_TAB = 0
private const val HOME_TAB = 1
private const val AUTOMATIONS_TAB = 2

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

/** A row in one of these states is still being determined; it asks for nothing yet and is not done. */
private val checkingCapabilityStates = setOf(CapabilityRowState.Starting, CapabilityRowState.Connecting)

/** Device facts read outside the Runtime: they change in system settings, so they are reread on every resume. */
private data class DeviceSetupState(
    val background: BackgroundFacts,
    val shizukuInstalled: Boolean,
    val notificationListenerGranted: Boolean,
)

@Composable
private fun rememberDeviceSetup(state: AppUiState): DeviceSetupState {
    val context = LocalContext.current
    val confirmations = state.backgroundConfirmations
    fun read() = DeviceSetupState(
        DeviceSetup.backgroundFacts(context, confirmations.autostart, confirmations.recentsLock),
        DeviceSetup.shizukuInstalled(context),
        DeviceSetup.notificationListenerGranted(
            context,
            (context.applicationContext as DroidBridgeApplication).requireAppGraph().notificationListener,
        ),
    )
    var facts by remember(confirmations) { mutableStateOf(read()) }
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { facts = read() }
    return facts
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
    // First setup left midway resumes at its chosen step, with the
    // steps before it still reachable by Back.
    val backStack = when {
        state.onboardingCompleted == true -> rememberNavBackStack(Main)
        state.setupRoute != null -> rememberNavBackStack(Welcome, SetupChoice, Capabilities)
        else -> rememberNavBackStack(Welcome)
    }
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
    // Only during first setup: its last step, the agent page, finishes it and lands on Home.
    val finishSetup: (() -> Unit)? = if (state.onboardingCompleted != true) {
        {
            viewModel.completeOnboarding()
            backStack.clear()
            backStack.add(Main)
        }
    } else {
        null
    }
    // Leaving first setup before it is finished is only ever a confirmed choice; this is what the
    // leave does once confirmed.
    var leavingSetup by remember { mutableStateOf<(() -> Unit)?>(null) }
    val activity = LocalActivity.current
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
            entry<Welcome> { WelcomeScreen { navigate(SetupChoice) } }
            entry<SetupChoice> {
                val setup = rememberDeviceSetup(state)
                SetupChoiceRoute(
                    route = state.setupRoute
                        ?: CapabilityRows.recommendedRoute(setup.shizukuInstalled),
                    selectRoute = viewModel::setSetupRoute,
                    agent = state.agentType,
                    selectAgent = viewModel::setAgentType,
                    facts = SetupChoiceFacts(setup.shizukuInstalled),
                    continueSetup = {
                        // The chosen route is committed here, so the guide that follows is built
                        // from a choice that survives leaving the App in the middle of it.
                        viewModel.setSetupRoute(
                            state.setupRoute
                                ?: CapabilityRows.recommendedRoute(setup.shizukuInstalled),
                        )
                        navigate(Capabilities)
                    },
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
                val rows = snapshot?.let { CapabilityRows.project(it, setup.notificationListenerGranted) }.orEmpty()
                val attention = rows.filter { it.action != null && it.state !in settledCapabilityStates } +
                    BackgroundRows.attention(setup.background, BackgroundRows.keeper(snapshot, state.keepAliveEnabled), connectionEnabled)
                // Without a snapshot nothing has been checked yet, which is not the same as all set.
                val checking = snapshot == null || rows.any { it.state in checkingCapabilityStates }
                PrimaryShell(
                    selected = selectedTab,
                    select = { selectedTab = it },
                    backToHome = backStack.size == 1,
                ) { page ->
                    when (page) {
                        HOME_TAB -> HomeRoute(
                            viewModel = home,
                            clientState = state.clientState,
                            attention = attention,
                            checking = checking,
                            onCapabilityAction = { row -> row.action?.let { capabilityAction(row.key, it) } },
                            newerVersionAvailable = updateState.newerVersionAvailable,
                            openTask = { taskId -> backStack.add(TaskDetail(taskId)) },
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
                        AUTOMATIONS_TAB -> AutomationsRoute(
                            viewModel = viewModel { AutomationListViewModel(graph.automations) },
                            openDetail = { id -> backStack.add(AutomationDetail(id)) },
                            openEditor = { backStack.add(AutomationEditor()) },
                        )
                        SETTINGS_TAB -> SettingsRoute(
                            theme = state.theme,
                            setTheme = viewModel::setTheme,
                            status = SettingsStatus(
                                pendingSetup = attention.size,
                                checking = checking,
                                agent = homeState.agentSummary(),
                                versionName = graph.apkVersionName,
                                newerVersionAvailable = updateState.newerVersionAvailable,
                                offersSetupGuide = true,
                            ),
                        ) { destination ->
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
                    // During first setup the chosen agent's page is the next step, on top of this one,
                    // so back returns here; setup finishes on that page. Later visits just return, and
                    // so does a setup left without a Runtime to configure anything against.
                    val ready = (state.clientState as? ClientState.Available)?.snapshot?.readiness ==
                        RuntimeReadiness.Ready
                    val next = when (state.agentType.takeIf { state.onboardingCompleted != true && ready }) {
                        AgentType.ChatGpt -> TunnelSetup
                        AgentType.LocalMcp -> MCP
                        else -> null
                    }
                    if (next != null) {
                        backStack.add(next)
                    } else if (state.onboardingCompleted != true) {
                        leavingSetup = {
                            viewModel.completeOnboarding()
                            navigate(Home)
                        }
                    } else {
                        navigate(Home)
                    }
                }, { backStack.removeLastOrNull() }, navigate)
            }
            entry<TaskDetail> { key ->
                TaskDetailRoute(viewModel { TaskDetailViewModel(graph.tasks, key.taskId) }) { backStack.removeLastOrNull() }
            }
            entry<AutomationDetail> { key ->
                AutomationDetailRoute(
                    viewModel = viewModel { AutomationDetailViewModel(graph.automations, key.automationId) },
                    edit = { backStack.add(AutomationEditor(key.automationId)) },
                    openTask = { taskId -> backStack.add(TaskDetail(taskId)) },
                ) { backStack.removeLastOrNull() }
            }
            entry<AutomationEditor> { key ->
                AutomationEditorRoute(
                    viewModel = viewModel { AutomationEditorViewModel(graph.automations, key.automationId) },
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
                    finishSetup = finishSetup,
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
                    finishSetup = finishSetup,
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
                    viewModel = viewModel { UpdatesViewModel(graph.client, graph.updates) },
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
    // Back from the guide's first page would close the App with setup unfinished.
    BackHandler(
        enabled = state.onboardingCompleted != true && backStack.size == 1 && backStack.first() == Welcome,
    ) { leavingSetup = { activity?.finish() } }
    leavingSetup?.let { leave ->
        AlertDialog(
            onDismissRequest = { leavingSetup = null },
            title = { Text(stringResource(AppR.string.setup_exit_title)) },
            text = { Text(stringResource(AppR.string.setup_exit_body)) },
            confirmButton = {
                TextButton(
                    onClick = { leavingSetup = null },
                    modifier = Modifier.testTag("setup_exit:continue"),
                ) { Text(stringResource(AppR.string.setup_exit_continue)) }
            },
            dismissButton = {
                TextButton(
                    onClick = {
                        leavingSetup = null
                        leave()
                    },
                    modifier = Modifier.testTag("setup_exit:leave"),
                ) { Text(stringResource(AppR.string.setup_exit_leave)) }
            },
        )
    }
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
                        Icon(
                            painterResource(destination.icon),
                            contentDescription = stringResource(destination.label),
                            modifier = Modifier.testTag(destination.tag),
                        )
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
            ) { Text(stringResource(AppR.string.welcome_start_setup)) }
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
                stringResource(AppR.string.welcome_description),
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
    val rows = snapshot?.let { CapabilityRows.project(it, setup.notificationListenerGranted) } ?: listOf(
        CapabilityRow(
            CapabilityRowKey.Runtime,
            if (state.clientState is ClientState.Unavailable) CapabilityRowState.Unavailable else CapabilityRowState.Starting,
            if (state.clientState is ClientState.Unavailable) CapabilityRows.runtimeAction(unavailableReason) else null,
            unavailableReason,
        ),
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
            // A Runtime that cannot start is repaired from Home, so first setup must not lock the
            // user out of it; only a Runtime that is still starting holds the button.
            val blocked = state.clientState is ClientState.Unavailable ||
                snapshot?.readiness == RuntimeReadiness.Unavailable
            Button(
                onClick = onEnter,
                enabled = snapshot?.readiness == RuntimeReadiness.Ready || blocked,
                modifier = Modifier.fillMaxWidth().navigationBarsPadding().padding(16.dp).heightIn(min = 56.dp)
                    .testTag("capabilities:enter"),
            ) {
                // During first setup this page is only the environment half of the guide; the agent
                // it was chosen for is configured next.
                Text(
                    stringResource(
                        when {
                            blocked -> AppR.string.capabilities_enter_anyway
                            state.onboardingCompleted != true -> AppR.string.action_continue
                            else -> AppR.string.capabilities_enter
                        },
                    ),
                )
            }
        },
    ) { padding ->
        val projected = BackgroundRows.project(setup.background, BackgroundRows.keeper(snapshot, state.keepAliveEnabled))
        // First setup asks only for the chosen route's steps; opened later, the page reports every
        // fact this device has, because a stronger backend may have arrived since.
        val steps = state.setupRoute
            ?.takeIf { state.onboardingCompleted != true }
            ?.let { CapabilityRows.steps(rows, projected, it) }
            ?: SetupSteps(rows, projected)
        val access = steps.access
        val background = steps.background
        val pending = (access + background).filter { it.action != null && it.state !in settledCapabilityStates }
        // The next step is the first open item; the others stay visible but quieter.
        val next = pending.firstOrNull()?.key
        LazyColumn(modifier = Modifier.fillMaxSize().padding(padding)) {
            item(key = "capabilities:summary") {
                // A step that is still starting asks for nothing yet, which is not the same as
                // being done with it.
                val waiting = (access + background).any { it.state in checkingCapabilityStates }
                Text(
                    when {
                        pending.isNotEmpty() ->
                            pluralStringResource(R.plurals.capabilities_remaining, pending.size, pending.size)
                        waiting -> stringResource(R.string.capabilities_checking)
                        else -> stringResource(R.string.capabilities_all_set)
                    },
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp).testTag("capabilities:summary"),
                )
            }
            item(key = "capabilities:section:access") { CapabilitySection(AppR.string.capabilities_section_access) }
            itemsIndexed(access, key = { _, row -> row.key }) { index, row ->
                CapabilityListItem(
                    row = row,
                    refreshing = row.key == CapabilityRowKey.Runtime && available?.refreshing == true,
                    emphasized = row.key == next,
                ) { row.action?.let { actionHandler(row.key, it) } }
                if (row.key == CapabilityRowKey.Shizuku && ShizukuManagers.plusInstalled(LocalContext.current)) {
                    TextButton(
                        onClick = { actionHandler(row.key, CapabilityAction.OpenShizuku) },
                        modifier = Modifier.padding(horizontal = 16.dp).heightIn(min = 48.dp).testTag("setup:shizuku:help"),
                    ) { Text(stringResource(AppR.string.shizuku_plus_help)) }
                }
                if (index != access.lastIndex) HorizontalDivider()
            }
            if (background.isNotEmpty()) {
                item(key = "capabilities:section:background") { CapabilitySection(AppR.string.capabilities_section_background) }
                itemsIndexed(background, key = { _, row -> row.key }) { index, row ->
                    CapabilityListItem(row = row, refreshing = false, emphasized = row.key == next) {
                        row.action?.let { actionHandler(row.key, it) }
                    }
                    if (index != background.lastIndex) HorizontalDivider()
                }
            }
        }
    }
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

private enum class SetupDialog { RestrictedSettings, AutostartConfirm, RecentsLock, ShizukuHelp }

@Composable
private fun rememberCapabilityActionHandler(
    viewModel: AppViewModel,
    navigate: (NavKey) -> Unit,
): (CapabilityRowKey, CapabilityAction) -> Unit {
    val context = LocalContext.current
    val notificationListener = remember(context) {
        (context.applicationContext as DroidBridgeApplication).requireAppGraph().notificationListener
    }
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
    val scope = rememberCoroutineScope()
    var shizukuOpenFailed by remember { mutableStateOf(false) }
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
        SetupDialog.ShizukuHelp -> AlertDialog(
            onDismissRequest = { dialog = null },
            title = { Text(stringResource(AppR.string.shizuku_setup_title)) },
            text = {
                Column {
                    Text(stringResource(
                        if (ShizukuManagers.plusInstalled(context)) AppR.string.shizuku_plus_setup_body
                        else AppR.string.shizuku_authorization_help,
                    ))
                    if (shizukuOpenFailed) Text(stringResource(AppR.string.shizuku_open_failed))
                }
            },
            confirmButton = {
                Column {
                    ShizukuManagers.launchers(context).forEach { (name, intent) ->
                        TextButton(onClick = {
                            try {
                                context.startActivity(intent)
                                dialog = null
                            } catch (_: android.content.ActivityNotFoundException) {
                                shizukuOpenFailed = true
                            } catch (_: SecurityException) {
                                shizukuOpenFailed = true
                            }
                        }) {
                            Text(stringResource(if (name == ShizukuManagers.PLUS_PACKAGE) AppR.string.shizuku_open_plus else AppR.string.shizuku_open_legacy))
                        }
                    }
                }
            },
            dismissButton = { TextButton(onClick = { dialog = null }) { Text(stringResource(R.string.action_cancel)) } },
        )
        SetupDialog.RestrictedSettings -> AlertDialog(
            onDismissRequest = { dialog = null },
            title = { Text(stringResource(AppR.string.restricted_settings_title)) },
            text = { Text(stringResource(AppR.string.restricted_settings_body)) },
            confirmButton = {
                TextButton(
                    onClick = { dialog = null; DeviceSetup.openAccessibility(context) },
                    modifier = Modifier.testTag("setup:restricted:continue"),
                ) { Text(stringResource(AppR.string.action_continue)) }
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
            title = { Text(stringResource(AppR.string.autostart_confirm_title)) },
            text = { Text(stringResource(AppR.string.autostart_confirm_body)) },
            confirmButton = {
                TextButton(
                    onClick = { dialog = null; viewModel.confirmAutostart() },
                    modifier = Modifier.testTag("setup:autostart:confirm"),
                ) { Text(stringResource(AppR.string.action_confirm_allowed)) }
            },
            dismissButton = { TextButton(onClick = { dialog = null }) { Text(stringResource(AppR.string.action_not_yet)) } },
        )
        SetupDialog.RecentsLock -> AlertDialog(
            onDismissRequest = { dialog = null },
            title = { Text(stringResource(AppR.string.recents_lock_title)) },
            text = { Text(stringResource(AppR.string.recents_lock_body)) },
            confirmButton = {
                TextButton(
                    onClick = { dialog = null; viewModel.confirmRecentsLock() },
                    modifier = Modifier.testTag("setup:recents:confirm"),
                ) { Text(stringResource(AppR.string.action_confirm_locked)) }
            },
            dismissButton = { TextButton(onClick = { dialog = null }) { Text(stringResource(R.string.action_cancel)) } },
        )
        null -> Unit
    }
    return { row, action ->
        when (action) {
            CapabilityAction.Retry, CapabilityAction.Recheck -> viewModel.recheckRuntime()
            CapabilityAction.Authorize -> scope.launch {
                if (!viewModel.requestShizukuAuthorization()) {
                    shizukuOpenFailed = false
                    dialog = SetupDialog.ShizukuHelp
                }
            }
            CapabilityAction.Diagnostics -> navigate(Diagnostics)
            CapabilityAction.InstallShizuku -> context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://shizuku.rikka.app/download/")))
            CapabilityAction.OpenShizuku -> {
                val launchers = ShizukuManagers.launchers(context)
                if (!ShizukuManagers.plusInstalled(context) && launchers.size == 1) {
                    try {
                        context.startActivity(launchers.single().second)
                    } catch (_: android.content.ActivityNotFoundException) {
                        shizukuOpenFailed = true
                        dialog = SetupDialog.ShizukuHelp
                    } catch (_: SecurityException) {
                        shizukuOpenFailed = true
                        dialog = SetupDialog.ShizukuHelp
                    }
                } else {
                    shizukuOpenFailed = launchers.isEmpty()
                    dialog = SetupDialog.ShizukuHelp
                }
            }
            CapabilityAction.Allow -> {
                if (row == CapabilityRowKey.LocalNetwork) {
                    if (Build.VERSION.SDK_INT >= 37) localNetwork.launch(localNetworkPermission())
                } else {
                    context.startActivity(Intent(AndroidSettings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM, Uri.parse("package:${context.packageName}")))
                }
            }
            CapabilityAction.OpenSettings -> when {
                row != CapabilityRowKey.Accessibility -> DeviceSetup.openNotificationAccess(context, notificationListener)
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
            CapabilityAction.TurnOnKeepAlive -> viewModel.setKeepAliveEnabled(true)
            CapabilityAction.TurnOffKeepAlive -> viewModel.setKeepAliveEnabled(false)
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
