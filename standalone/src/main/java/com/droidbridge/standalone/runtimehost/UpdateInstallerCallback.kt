package com.droidbridge.standalone.runtimehost

internal data class UpdateInstallerCallback(
    val updateId: String,
    val attemptId: String,
    val sessionId: Int,
    val status: Int?,
) {
    fun matches(record: UpdateMaintenanceRecord): Boolean =
        record.updateId == updateId && record.lastAttempt?.let {
            it.id == attemptId && it.sessionId == sessionId && sessionId >= 0
        } == true
}

internal fun installerFailureCode(status: Int): String? = when (status) {
    1 -> "INSTALLER_FAILURE"
    2 -> "INSTALLER_BLOCKED"
    3 -> "INSTALLER_ABORTED"
    4 -> "INSTALLER_INVALID_APK"
    5 -> "INSTALLER_CONFLICT"
    6 -> "INSTALLER_STORAGE"
    7 -> "INSTALLER_INCOMPATIBLE"
    8 -> "INSTALLER_TIMEOUT"
    else -> null
}
