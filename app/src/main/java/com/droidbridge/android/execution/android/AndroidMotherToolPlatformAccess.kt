package com.droidbridge.android.execution.android

import android.app.ActivityOptions
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.IntentSender
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle

/**
 * Sender opt-in that lets Android apply DroidBridge's own background-activity-start
 * privilege to an IntentSender/PendingIntent it sends (API 34+). It grants nothing the
 * process does not already hold and is not caller data.
 */
internal object BackgroundStartOptions {
    fun sender(): Bundle? {
        if (Build.VERSION.SDK_INT < 34) return null
        val mode = if (Build.VERSION.SDK_INT >= 36) {
            ActivityOptions.MODE_BACKGROUND_ACTIVITY_START_ALLOW_ALWAYS
        } else {
            @Suppress("DEPRECATION")
            ActivityOptions.MODE_BACKGROUND_ACTIVITY_START_ALLOWED
        }
        return ActivityOptions.makeBasic()
            .setPendingIntentBackgroundActivityStartMode(mode)
            .toBundle()
    }
}

internal class AndroidMotherToolPlatformAccess(
    private val context: Context,
) : AndroidMotherToolAccess {
    override fun inspectPackage(packageName: String): VisiblePackageFact? {
        val packageManager = context.packageManager
        val info = try {
            packageManager.getPackageInfo(packageName, PackageManager.PackageInfoFlags.of(0))
        } catch (_: PackageManager.NameNotFoundException) {
            return null
        }
        val application = info.applicationInfo ?: throw AndroidExecutionException("IO_ERROR")
        return VisiblePackageFact(
            packageName = info.packageName,
            versionName = info.versionName,
            versionCode = info.longVersionCode,
            enabled = application.enabled,
            system = application.flags and ApplicationInfo.FLAG_SYSTEM != 0,
            launchable = packageManager.getLaunchIntentForPackage(packageName) != null,
        )
    }

    override fun launchPackage(packageName: String) {
        val sender = context.packageManager.getLaunchIntentSenderForPackage(packageName)
        try {
            context.startIntentSender(sender, null, 0, 0, 0, BackgroundStartOptions.sender())
        } catch (_: IntentSender.SendIntentException) {
            throw AndroidExecutionException("NOT_FOUND")
        }
    }

    override fun startActivity(start: ActivityStart) {
        val intent = Intent(start.action)
        start.dataUri?.let { intent.data = Uri.parse(it) }
        when {
            start.className != null -> intent.setClassName(
                start.packageName ?: throw AndroidExecutionException("INVALID_ARGUMENT"),
                start.className,
            )
            start.packageName != null -> intent.setPackage(start.packageName)
        }
        start.extras.forEach { (key, value) ->
            when (value) {
                is String -> intent.putExtra(key, value)
                is Boolean -> intent.putExtra(key, value)
                is Long -> intent.putExtra(key, value)
                else -> throw AndroidExecutionException("INVALID_ARGUMENT")
            }
        }
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            context.startActivity(intent)
        } catch (_: ActivityNotFoundException) {
            throw AndroidExecutionException("NOT_FOUND")
        }
    }

    override fun readClipboardText(): String? {
        val clip = clipboard().primaryClip ?: return null
        if (clip.itemCount == 0) return null
        return clip.getItemAt(0).text?.toString()
    }

    override fun writeClipboardText(text: String) {
        clipboard().setPrimaryClip(ClipData.newPlainText("", text))
    }

    override fun clearClipboard() {
        clipboard().clearPrimaryClip()
    }

    private fun clipboard(): ClipboardManager =
        context.getSystemService(ClipboardManager::class.java)
            ?: throw AndroidExecutionException("CAPABILITY_UNAVAILABLE")
}
