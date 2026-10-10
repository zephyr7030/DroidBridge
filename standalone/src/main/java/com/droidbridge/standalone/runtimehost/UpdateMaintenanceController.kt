package com.droidbridge.standalone.runtimehost

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.content.ActivityNotFoundException
import java.io.IOException
import java.util.UUID
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import com.droidbridge.standalone.product.release.ReleaseArtifact
import com.droidbridge.standalone.product.release.ReleaseConfig
import com.droidbridge.standalone.product.release.ReleaseHash
import com.droidbridge.standalone.product.release.ReleaseManifests
import com.droidbridge.standalone.product.release.ReleaseRejected
import com.droidbridge.standalone.product.update.cleanupUpdateCache
import java.io.File
import java.security.MessageDigest
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** HostController facilities the maintenance flow needs; every call runs on its transition executor. */
internal interface MaintenanceHost {
    /** Makes APK Runtime the active host; returns a refusal code or null. */
    fun ensureApkHost(): String?

    /** Closes business admission after proving zero work; returns a refusal code or null. */
    fun closeAdmission(): String?

    fun reopenAdmission(): Boolean
}

internal data class ArchiveFacts(val packageName: String, val versionCode: Long, val signerSha256: String?)

internal interface InstalledPackageFacts {
    fun installedVersionCode(): Long

    fun installedSignerSha256(): String?

    /** Package identity of an APK file, or null when Android cannot parse it. */
    fun archive(file: File): ArchiveFacts?
}

internal interface ApkSessionInstaller {
    fun abandonUnrecordedSessions(recorded: Int?)

    fun create(size: Long): Int

    fun exists(sessionId: Int): Boolean

    /** Streams exactly the verified bytes into the session, then commits it for user confirmation. */
    fun writeAndCommit(updateId: String, attempt: UpdateInstallAttempt, apk: File, size: Long, sha256: String)

    fun abandon(sessionId: Int)
}

private class MaintenanceRefused(val code: String, message: String) : Exception(message)

/**
 * The APK `:runtime` S-UPD-002..004 maintenance flow. The signed manifest is verified again here and
 * artifacts are read only from the deterministic update-cache path, never from a caller path.
 */
internal class UpdateMaintenanceController(
    private val packageName: String,
    private val config: ReleaseConfig,
    private val store: UpdateMaintenanceStore,
    private val host: MaintenanceHost,
    private val packages: InstalledPackageFacts,
    private val installer: ApkSessionInstaller,
    private val cacheRoot: File,
    private val nowMillis: () -> Long = System::currentTimeMillis,
) {
    fun state(): String = reply("read_maintenance") {
        val record = pruneFailure(store.read())
        buildJsonObject {
            put("schema_version", 1)
            put("configured", config is ReleaseConfig.Configured)
            put("installed_version_code", packages.installedVersionCode())
            put("record", record?.let { Json.parseToJsonElement(it.encode()) } ?: JsonNull)
        }.toString()
    }

    fun beginProductUpdate(manifestBytes: ByteArray, signature: ByteArray): String = reply("release_configuration") {
        val configured = configured()
        stage = "verify_manifest"
        val manifest = verified { ReleaseManifests.verify(configured, manifestBytes, signature) }
        if (manifest.versionCode <= packages.installedVersionCode()) refuse(INVALID_ARGUMENT, "release is not newer")
        stage = "verify_apk"
        val apk = verifiedArtifact(manifest.version, manifest.apk)
        val archive = packages.archive(apk) ?: refuse(INVALID_ARGUMENT, "APK cannot be parsed")
        if (archive.packageName != packageName || archive.versionCode != manifest.versionCode ||
            archive.signerSha256 != configured.apkSignerSha256
        ) {
            refuse(INVALID_ARGUMENT, "APK identity does not match the signed release")
        }
        val record = UpdateMaintenanceRecord(
            updateId = UpdateMaintenanceRecord.newUpdateId(),
            targetVersion = manifest.version,
            targetVersionCode = manifest.versionCode,
            targetApkSha256 = manifest.apk.sha256,
            targetApkSize = manifest.apk.size,
            targetApkSignerSha256 = configured.apkSignerSha256,
            phase = MaintenancePhase.Prepared,
            apkSessionId = null,
        )
        stage = "enter_maintenance"
        enterMaintenance(record, this)
    }

    /** One explicit APK attempt from `prepared` through the platform package installer. */
    fun installApk(updateId: String): String = reply("validate_install") {
        val record = current(updateId)
        if (record.phase != MaintenancePhase.Prepared) refuse(INVALID_ARGUMENT, "APK install is legal only from prepared")
        val apk = cachedFile(record.targetVersion, apkName(record.targetVersion))
        if (!matches(apk, record.targetApkSize, record.targetApkSha256)) refuse(INVALID_ARGUMENT, "verified APK is no longer cached")
        installWithPackageInstaller(record, apk, this)
    }

    fun cancel(updateId: String): String = reply("validate_cancel") {
        val record = current(updateId)
        if (targetApkInstalled(record)) refuse(HOST_TRANSITION_PENDING, "the installed APK must be reconciled first")
        stage = "abandon_session"
        record.apkSessionId?.let(installer::abandon)
        stage = "delete_maintenance"
        store.delete(record)
        stage = "reopen_admission"
        reopen()
        success(null)
    }

    /**
     * Observation-driven reconciliation after restart, package replacement or an Updates read. It
     * never starts an install and never infers success from callbacks.
     */
    fun recover() {
        val record = pruneFailure(store.read()) ?: return
        if (targetApkInstalled(record)) {
            installer.abandonUnrecordedSessions(null)
            advanceAfterApk(record)
            return
        }
        when (record.phase) {
            MaintenancePhase.Prepared -> installer.abandonUnrecordedSessions(null)
            MaintenancePhase.ApkInstalling -> {
                installer.abandonUnrecordedSessions(record.apkSessionId)
                val live = record.apkSessionId?.let(installer::exists) == true
                if (!live) store.replace(record, record.copy(
                    phase = MaintenancePhase.Prepared,
                    apkSessionId = null,
                    lastAttempt = record.lastAttempt?.let { attempt ->
                        if (attempt.terminalCallbackSeen || attempt.failure != null) attempt else attempt.copy(
                            failure = UpdateInstallFailure("INSTALLER_RESULT_MISSING", "installer_result", nowMillis().coerceAtLeast(0)),
                        )
                    },
                ))
            }
        }
    }

    fun refresh(): String = reply("recover_maintenance") {
        recover()
        stage = "cleanup_downloads"
        cleanupDownloads()
        state()
    }

    /** Maintenance operations and cache cleanup share the same executor and record observation. */
    fun cleanupDownloads() {
        val record = store.read()
        val referenced = record?.let { setOf(cachedFile(it.targetVersion, apkName(it.targetVersion))) } ?: emptySet()
        cleanupUpdateCache(cacheRoot, referenced, nowMillis())
    }

    private fun installWithPackageInstaller(record: UpdateMaintenanceRecord, apk: File, action: UpdateAction): String {
        action.stage = "abandon_unrecorded_sessions"
        installer.abandonUnrecordedSessions(null)
        action.stage = "create_session"
        val sessionId = installer.create(record.targetApkSize)
        val attempt = UpdateInstallAttempt(UUID.randomUUID().toString(), sessionId)
        val installing = record.copy(phase = MaintenancePhase.ApkInstalling, apkSessionId = sessionId, lastAttempt = attempt)
        action.stage = "record_session"
        try {
            store.replace(record, installing)
        } catch (error: Exception) {
            installer.abandon(sessionId)
            throw error
        }
        action.stage = "apk_write_commit"
        try {
            installer.writeAndCommit(record.updateId, attempt, apk, record.targetApkSize, record.targetApkSha256)
        } catch (error: Exception) {
            val failure = operationFailure(error, action.stage)
            val failed = installing.copy(lastAttempt = attempt.copy(
                terminalCallbackSeen = true,
                failure = UpdateInstallFailure(failure.code, failure.stage, nowMillis().coerceAtLeast(0)),
            ))
            action.stage = "record_install_failure"
            try {
                try {
                    store.replace(installing, failed)
                } catch (recordError: Exception) {
                    throw operationFailure(recordError, "record_install_failure")
                }
            } finally {
                action.stage = "abandon_session"
                installer.abandon(sessionId)
            }
            action.stage = "restore_prepared"
            store.replace(failed, failed.copy(phase = MaintenancePhase.Prepared, apkSessionId = null))
            throw failure
        }
        return success(installing)
    }

    /** Called only on the maintenance executor; callback identities never create maintenance. */
    fun installerResult(
        callback: UpdateInstallerCallback,
        canHandle: () -> Boolean,
        confirmation: (() -> Unit)?,
    ): Boolean {
        if (!canHandle()) return false
        val record = store.read() ?: return false
        if (!callback.matches(record) || !canHandle()) return false
        val attempt = requireNotNull(record.lastAttempt)
        if (attempt.terminalCallbackSeen) return false
        if (callback.status == -1) {
            if (record.phase != MaintenancePhase.ApkInstalling || record.apkSessionId != callback.sessionId ||
                attempt.confirmationHandled || !installer.exists(callback.sessionId) || !canHandle()
            ) return false
            val requested = record.copy(lastAttempt = attempt.copy(confirmationHandled = true))
            store.replace(record, requested)
            if (!canHandle()) {
                store.replace(requested, requested.copy(lastAttempt = requested.lastAttempt!!.copy(
                    failure = UpdateInstallFailure("CALLBACK_TIMEOUT", "installer_confirmation", nowMillis().coerceAtLeast(0)),
                )))
                return false
            }
            try {
                if (confirmation == null) throw UpdateOperationFailure("CONFIRMATION_MISSING", "installer_confirmation")
                confirmation()
            } catch (error: Exception) {
                val failure = operationFailure(error, "installer_confirmation")
                store.replace(requested, requested.copy(lastAttempt = requested.lastAttempt!!.copy(
                    failure = UpdateInstallFailure(failure.code, failure.stage, nowMillis().coerceAtLeast(0)),
                )))
            }
            return true
        }
        val code = callback.status?.let(::installerFailureCode)
        val failure = when {
            callback.status == 0 -> null
            code != null -> UpdateInstallFailure(code, "installer_result", nowMillis().coerceAtLeast(0))
            else -> UpdateInstallFailure(
                if (callback.status == null) "INSTALLER_STATUS_MISSING" else "INSTALLER_STATUS_UNKNOWN",
                "installer_result", nowMillis().coerceAtLeast(0),
            )
        }
        if (!canHandle()) return false
        store.replace(record, record.copy(lastAttempt = attempt.copy(
            terminalCallbackSeen = callback.status == 0 || code != null,
            failure = failure,
        )))
        return true
    }

    private fun pruneFailure(record: UpdateMaintenanceRecord?): UpdateMaintenanceRecord? {
        val failure = record?.lastAttempt?.failure ?: return record
        val now = nowMillis()
        if (now >= failure.atMillis && now - failure.atMillis <= FAILURE_MAX_AGE_MS) return record
        val next = record.copy(lastAttempt = record.lastAttempt.copy(failure = null))
        store.replace(record, next)
        return next
    }

    private fun advanceAfterApk(record: UpdateMaintenanceRecord) {
        store.delete(record)
        reopen()
    }

    private fun enterMaintenance(record: UpdateMaintenanceRecord, action: UpdateAction): String {
        if (store.read() != null) refuse(HOST_TRANSITION_PENDING, "update maintenance is already recorded")
        action.stage = "activate_apk_host"
        host.ensureApkHost()?.let { refuse(it, "APK Runtime is not the active host") }
        action.stage = "close_admission"
        host.closeAdmission()?.let { refuse(it, "business admission could not close") }
        action.stage = "record_maintenance"
        try {
            store.create(record)
        } catch (error: Exception) {
            host.reopenAdmission()
            throw error
        }
        return success(record)
    }

    private fun reopen() {
        if (!host.reopenAdmission()) refuse(HOST_TRANSITION_PENDING, "business admission did not reopen")
    }

    private fun targetApkInstalled(record: UpdateMaintenanceRecord): Boolean =
        packages.installedVersionCode() == record.targetVersionCode &&
            packages.installedSignerSha256() == record.targetApkSignerSha256

    private fun configured(): ReleaseConfig.Configured =
        config as? ReleaseConfig.Configured ?: refuse(CAPABILITY_UNAVAILABLE, "release configuration is unavailable")

    private fun current(updateId: String): UpdateMaintenanceRecord {
        val record = store.read() ?: refuse(STALE_AUTHORITY, "no update maintenance is recorded")
        if (record.updateId != updateId) refuse(STALE_AUTHORITY, "update maintenance identity changed")
        return record
    }

    private fun verifiedArtifact(version: String, artifact: ReleaseArtifact): File {
        val file = cachedFile(version, artifact.name)
        if (!matches(file, artifact.size, artifact.sha256)) refuse(INVALID_ARGUMENT, "${artifact.name} is not a verified download")
        return file
    }

    private fun apkName(version: String) = "droidbridge-$version-arm64-v8a.apk"

    private fun cachedFile(version: String, name: String) = File(File(cacheRoot, version), name)

    private fun matches(file: File, size: Long, sha256: String): Boolean =
        file.isFile && file.length() == size && ReleaseHash.sha256(file) == sha256

    private fun <T> verified(block: () -> T): T = try {
        block()
    } catch (rejected: ReleaseRejected) {
        refuse(INVALID_ARGUMENT, rejected.message.orEmpty())
    }

    private fun success(record: UpdateMaintenanceRecord?): String = buildJsonObject {
        put("schema_version", 1)
        put("record", record?.let { Json.parseToJsonElement(it.encode()) } ?: JsonNull)
    }.toString()

    private class UpdateAction(var stage: String)

    private inline fun reply(stage: String, block: UpdateAction.() -> String): String {
        val action = UpdateAction(stage)
        return try {
            action.block()
        } catch (refused: MaintenanceRefused) {
            failure(refused.code, action.stage)
        } catch (error: Exception) {
            val problem = operationFailure(error, action.stage)
            failure(problem.code, problem.stage)
        }
    }

    private fun failure(code: String, stage: String): String = buildJsonObject {
        put("schema_version", 1)
        put("error", code)
        put("stage", stage)
    }.toString()

    private fun refuse(code: String, message: String): Nothing = throw MaintenanceRefused(code, message)

    private companion object {
        const val INVALID_ARGUMENT = "INVALID_ARGUMENT"
        const val CAPABILITY_UNAVAILABLE = "CAPABILITY_UNAVAILABLE"
        const val HOST_TRANSITION_PENDING = "HOST_TRANSITION_PENDING"
        const val STALE_AUTHORITY = "STALE_AUTHORITY"
        const val IO_ERROR = "IO_ERROR"
        const val FAILURE_MAX_AGE_MS = 24L * 60 * 60 * 1000
    }
}

internal class AndroidPackageFacts(private val context: Context) : InstalledPackageFacts {
    private val signingFlags = PackageManager.PackageInfoFlags.of(PackageManager.GET_SIGNING_CERTIFICATES.toLong())

    override fun installedVersionCode(): Long =
        context.packageManager.getPackageInfo(context.packageName, PackageManager.PackageInfoFlags.of(0)).longVersionCode

    override fun installedSignerSha256(): String? =
        context.packageManager.getPackageInfo(context.packageName, signingFlags).signingInfo?.let(::singleSigner)

    override fun archive(file: File): ArchiveFacts? =
        context.packageManager.getPackageArchiveInfo(file.absolutePath, signingFlags)?.let { info ->
            ArchiveFacts(info.packageName, info.longVersionCode, info.signingInfo?.let(::singleSigner))
        }

    private fun singleSigner(info: android.content.pm.SigningInfo): String? =
        info.takeUnless { it.hasMultipleSigners() }?.apkContentsSigners?.singleOrNull()?.let { signer ->
            MessageDigest.getInstance("SHA-256").digest(signer.toByteArray()).joinToString("") { "%02x".format(it) }
        }
}

internal class AndroidApkSessionInstaller(private val context: Context) : ApkSessionInstaller {
    private val installer = context.packageManager.packageInstaller

    override fun abandonUnrecordedSessions(recorded: Int?) {
        installer.mySessions
            .filter { it.appPackageName == context.packageName && it.sessionId != recorded }
            .forEach { installer.abandonSession(it.sessionId) }
    }

    override fun create(size: Long): Int = installer.createSession(
        PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
            setAppPackageName(context.packageName)
            setSize(size)
        },
    )

    override fun exists(sessionId: Int): Boolean = installer.getSessionInfo(sessionId) != null

    override fun writeAndCommit(updateId: String, attempt: UpdateInstallAttempt, apk: File, size: Long, sha256: String) {
        var stage = "apk_write"
        try {
            installer.openSession(attempt.sessionId).use { session ->
                val digest = MessageDigest.getInstance("SHA-256")
                var written = 0L
                apk.inputStream().use { input ->
                    session.openWrite(BASE_APK, 0, size).use { output ->
                        val buffer = ByteArray(64 * 1024)
                        while (true) {
                            val read = input.read(buffer)
                            if (read < 0) break
                            digest.update(buffer, 0, read)
                            output.write(buffer, 0, read)
                            written += read
                        }
                        session.fsync(output)
                    }
                }
                val actual = digest.digest().joinToString("") { "%02x".format(it) }
                if (written != size || actual != sha256) throw UpdateOperationFailure("INVALID_ARGUMENT", stage)
                stage = "installer_commit"
                // The platform installer adds its status extras, so this explicit broadcast must be mutable.
                val result = PendingIntent.getBroadcast(
                    context,
                    attempt.sessionId,
                    Intent(context, PackageInstallerResultReceiver::class.java)
                        .setPackage(context.packageName)
                        .setData(Uri.Builder().scheme("droidbridge-update").authority("install")
                            .appendPath(updateId).appendPath(attempt.id).build()),
                    PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                )
                session.commit(result.intentSender)
            }
        } catch (error: Exception) {
            throw operationFailure(error, stage)
        }
    }

    override fun abandon(sessionId: Int) {
        if (installer.getSessionInfo(sessionId) != null) installer.abandonSession(sessionId)
    }

    private companion object {
        const val BASE_APK = "base.apk"
    }
}

private class UpdateOperationFailure(val code: String, val stage: String) : Exception()

private fun operationFailure(error: Exception, stage: String): UpdateOperationFailure =
    error as? UpdateOperationFailure ?: UpdateOperationFailure(
        when (error) {
            is SecurityException -> "PERMISSION_DENIED"
            is ActivityNotFoundException -> "ACTIVITY_NOT_FOUND"
            is IllegalArgumentException -> "INVALID_ARGUMENT"
            is MaintenanceStoreConflict -> "STALE_AUTHORITY"
            is IllegalStateException -> "IO_ERROR"
            is IOException -> "IO_ERROR"
            else -> "IO_ERROR"
        },
        stage,
    )
