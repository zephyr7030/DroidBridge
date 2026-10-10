package com.droidbridge.ui.client

enum class CapabilityRowKey {
    Runtime,
    RootBackend,
    Shizuku,
    LocalNetwork,
    NotificationAccess,
    ExactAlarm,
    Accessibility,
    ScreenCapture,
    BackgroundKeeper,
    BatteryOptimization,
    BackgroundRestriction,
    VendorAutostart,
    RecentsLock,
}

enum class CapabilityRowState {
    Ready,
    Starting,
    Unavailable,
    NotInstalled,
    CompatRequired,
    UpdateRequired,
    NotRunning,
    NotAuthorized,
    Connecting,
    Connected,
    IncompatibleIdentity,
    NotAllowed,
    Active,
    Unknown,
    KeptByModule,
    KeptByShizuku,
    /** The user turned Shizuku keep-alive off. */
    KeepAliveOff,
    NotConfirmed,
    Confirmed,
}

enum class CapabilityAction {
    Retry,
    Recheck,
    Allow,
    Diagnostics,
    InstallShizuku,
    OpenShizuku,
    Authorize,
    OpenSettings,
    StartCapture,
    StopCapture,
    AllowBattery,
    OpenAppDetails,
    OpenAutostart,
    ShowRecentsLockHelp,
    TurnOnKeepAlive,
    TurnOffKeepAlive,
}

data class CapabilityRow(
    val key: CapabilityRowKey,
    val state: CapabilityRowState,
    val action: CapabilityAction? = null,
    val reason: String? = null,
)

/** A row in one of these states asks the user for nothing. */
val settledCapabilityStates = setOf(
    CapabilityRowState.Ready,
    CapabilityRowState.Connected,
    CapabilityRowState.Active,
    CapabilityRowState.KeptByModule,
    CapabilityRowState.KeptByShizuku,
    CapabilityRowState.KeepAliveOff,
    CapabilityRowState.Confirmed,
)

/**
 * How the user chose to let DroidBridge act on this phone, picked once during first setup. It
 * shapes the setup guide only: afterwards the capabilities page reports every fact the device
 * has, since a stronger backend may be installed long after setup.
 */
enum class SetupRoute(val wireValue: String) {
    Shizuku("shizuku"),
    AccessibilityOnly("accessibility_only"),
}
