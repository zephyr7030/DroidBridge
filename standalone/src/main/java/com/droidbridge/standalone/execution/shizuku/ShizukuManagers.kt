package com.droidbridge.standalone.execution.shizuku

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager

/** Package discovery is guidance only; it never identifies the owner of a received Binder. */
internal object ShizukuManagers {
    const val LEGACY_PACKAGE = "moe.shizuku.privileged.api"
    const val PLUS_PACKAGE = "af.shizuku.plus.api"

    fun legacyInstalled(context: Context) = installed(context, LEGACY_PACKAGE)
    fun plusInstalled(context: Context) = installed(context, PLUS_PACKAGE)
    fun anyInstalled(context: Context) = legacyInstalled(context) || plusInstalled(context)

    fun launchers(context: Context): List<Pair<String, Intent>> =
        listOf(LEGACY_PACKAGE, PLUS_PACKAGE).mapNotNull { name ->
            context.packageManager.getLaunchIntentForPackage(name)?.let { name to it }
        }

    private fun installed(context: Context, name: String): Boolean = try {
        context.packageManager.getApplicationInfo(name, PackageManager.ApplicationInfoFlags.of(0))
        true
    } catch (_: PackageManager.NameNotFoundException) {
        false
    }
}
