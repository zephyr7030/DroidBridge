package com.droidbridge.android.product.update

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller

/**
 * Receives only this App's request-scoped self-update session status. It launches the platform
 * confirmation when the installer asks for it; the outcome itself is observed later from the
 * installed package and session state, never from this callback (S-UPD-002).
 */
class PackageInstallerResultReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE) !=
            PackageInstaller.STATUS_PENDING_USER_ACTION
        ) {
            return
        }
        val confirmation = intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java) ?: return
        context.startActivity(confirmation.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }
}
