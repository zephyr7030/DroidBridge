package com.droidbridge.android.execution.shizuku

import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import com.droidbridge.android.execution.android.AndroidExecutionRequest
import com.droidbridge.android.execution.android.AndroidExecutionResult
import com.droidbridge.android.execution.android.RoleDescriptor
import java.io.FileDescriptor
import java.util.Base64
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import org.json.JSONObject

internal data class ShizukuFsResult(
    val payload: ByteArray,
    val descriptor: ParcelFileDescriptor? = null,
)

internal object ShizukuFsExecutor {
    fun execute(
        operation: ShizukuFsOperation,
        inputDescriptor: ParcelFileDescriptor?,
    ): ShizukuFsResult = when (operation) {
        is ShizukuFsOperation.Lstat -> lstat(operation.path)
        is ShizukuFsOperation.AccessWriteSearch -> {
            if (!Os.access(operation.path, OsConstants.W_OK or OsConstants.X_OK)) {
                throw ErrnoException("access", OsConstants.EACCES)
            }
            ShizukuFsResult(completed())
        }
        is ShizukuFsOperation.OpenRead -> open(
            operation.path,
            OsConstants.O_RDONLY or OsConstants.O_CLOEXEC or OsConstants.O_NOFOLLOW,
            0,
        )
        is ShizukuFsOperation.ReadBounded -> readBounded(operation.path, operation.limit)
        is ShizukuFsOperation.ReadDirectory -> {
            val payload = checkNotNull(
                ShizukuNativeLauncher.nativeReadDirectory(
                    operation.path,
                    operation.cookie,
                    operation.limit,
                ),
            )
            val result = JSONObject(payload.toString(Charsets.UTF_8))
            if (result.has("errno")) {
                throw ErrnoException("getdents64", result.getInt("errno"))
            }
            ShizukuFsResult(payload)
        }
        is ShizukuFsOperation.CreateExclusive -> open(
            operation.path,
            OsConstants.O_WRONLY or OsConstants.O_CREAT or OsConstants.O_EXCL or
                OsConstants.O_CLOEXEC or OsConstants.O_NOFOLLOW,
            operation.mode,
        )
        is ShizukuFsOperation.OpenWrite -> open(
            operation.path,
            OsConstants.O_WRONLY or OsConstants.O_CLOEXEC or OsConstants.O_NOFOLLOW,
            0,
        )
        ShizukuFsOperation.FsyncDescriptor -> {
            Os.fsync(requireNotNull(inputDescriptor).fileDescriptor)
            ShizukuFsResult(completed())
        }
        is ShizukuFsOperation.ApplyMetadata -> {
            val descriptor = requireNotNull(inputDescriptor)
            val stat = Os.fstat(descriptor.fileDescriptor)
            if (stat.st_uid != operation.uid || stat.st_gid != operation.gid) {
                Os.fchown(descriptor.fileDescriptor, operation.uid, operation.gid)
            }
            if ((stat.st_mode and MODE_MASK) != (operation.mode and MODE_MASK)) {
                Os.fchmod(descriptor.fileDescriptor, operation.mode)
            }
            operation.selinuxContext?.let { context ->
                val descriptorPath = "/proc/self/fd/${descriptor.fd}"
                val currentContext = try {
                    Os.getxattr(descriptorPath, SELINUX_XATTR)
                } catch (error: ErrnoException) {
                    if (error.errno == OsConstants.ENODATA || error.errno == OsConstants.ENOTSUP) {
                        null
                    } else {
                        throw error
                    }
                }
                if (currentContext == null || !currentContext.contentEquals(context)) {
                    try {
                        Os.setxattr(descriptorPath, SELINUX_XATTR, context, 0)
                    } catch (error: ErrnoException) {
                        // A filesystem that cannot store a label — shared storage is one — keeps
                        // the mount's own label, which is what every file there carries.
                        if (error.errno != OsConstants.ENOTSUP) {
                            throw error
                        }
                    }
                }
            }
            ShizukuFsResult(completed())
        }
        is ShizukuFsOperation.RenameAtomic -> {
            val errno = ShizukuNativeLauncher.nativeRename(
                operation.source,
                operation.destination,
                operation.exchange,
            )
            if (errno in setOf(
                    OsConstants.EINVAL,
                    OsConstants.ENOSYS,
                    OsConstants.EOPNOTSUPP,
                    OsConstants.EXDEV,
                )
            ) {
                throw UnsupportedOperationException("atomic rename is unsupported")
            }
            if (errno != 0) throw ErrnoException("renameat2", errno)
            ShizukuFsResult(completed())
        }
        is ShizukuFsOperation.FsyncDirectory -> {
            fsyncDirectory(operation.path)
            ShizukuFsResult(completed())
        }
        is ShizukuFsOperation.Mkdir -> {
            Os.mkdir(operation.path, operation.mode)
            ShizukuFsResult(completed())
        }
        is ShizukuFsOperation.Unlink -> {
            Os.remove(operation.path)
            ShizukuFsResult(completed())
        }
        is ShizukuFsOperation.Readlink -> ShizukuFsResult(
            JSONObject().put("target", Os.readlink(operation.path)).toString().toByteArray(),
        )
        is ShizukuFsOperation.Symlink -> {
            Os.symlink(operation.target, operation.destination)
            ShizukuFsResult(completed())
        }
    }

    fun errorCode(error: Throwable): String {
        val errno = (error as? ErrnoException)?.errno
        return when (errno) {
            OsConstants.EACCES, OsConstants.EPERM -> "PERMISSION_DENIED"
            OsConstants.ENOENT -> "NOT_FOUND"
            OsConstants.EEXIST -> "ALREADY_EXISTS"
            OsConstants.ENOTEMPTY -> "NOT_EMPTY"
            else -> when (error) {
                is SecurityException -> "PERMISSION_DENIED"
                is UnsupportedOperationException -> "UNSUPPORTED"
                is IllegalArgumentException, is NullPointerException -> "INVALID_ARGUMENT"
                else -> "IO_ERROR"
            }
        }
    }

    private fun lstat(path: String): ShizukuFsResult {
        val stat = Os.lstat(path)
        val result = JSONObject()
            .put("device", stat.st_dev)
            .put("inode", stat.st_ino)
            .put("mode", stat.st_mode)
            .put("uid", stat.st_uid)
            .put("gid", stat.st_gid)
            .put("size", stat.st_size)
            .put("modified_at_epoch_seconds", stat.st_mtime)
        val context = try {
            Os.getxattr(path, SELINUX_XATTR)
        } catch (error: ErrnoException) {
            if (error.errno == OsConstants.ENODATA || error.errno == OsConstants.ENOTSUP) {
                null
            } else {
                throw error
            }
        }
        if (context != null) {
            result.put("selinux_context_base64", Base64.getEncoder().encodeToString(context))
        }
        return ShizukuFsResult(result.toString().toByteArray())
    }

    private fun open(path: String, flags: Int, mode: Int): ShizukuFsResult {
        val descriptor = Os.open(path, flags, mode)
        return try {
            ShizukuFsResult(completed(), ParcelFileDescriptor.dup(descriptor))
        } finally {
            Os.close(descriptor)
        }
    }

    private fun readBounded(path: String, limit: Int): ShizukuFsResult {
        val descriptor = Os.open(
            path,
            OsConstants.O_RDONLY or OsConstants.O_CLOEXEC or OsConstants.O_NOFOLLOW,
            0,
        )
        val buffer = ByteArray(limit + 1)
        var filled = 0
        try {
            while (filled < buffer.size) {
                val count = Os.read(descriptor, buffer, filled, buffer.size - filled)
                if (count <= 0) break
                filled += count
            }
        } finally {
            Os.close(descriptor)
        }
        val truncated = filled > limit
        val content = buffer.copyOf(minOf(filled, limit))
        return ShizukuFsResult(
            JSONObject()
                .put("content_base64", Base64.getEncoder().encodeToString(content))
                .put("truncated", truncated)
                .toString()
                .toByteArray(),
        )
    }

    private fun fsyncDirectory(path: String) {
        val descriptor: FileDescriptor = Os.open(
            path,
            OsConstants.O_RDONLY or OsConstants.O_CLOEXEC or OsConstants.O_NOFOLLOW,
            0,
        )
        try {
            if (!OsConstants.S_ISDIR(Os.fstat(descriptor).st_mode)) {
                throw ErrnoException("fsync", OsConstants.ENOTDIR)
            }
            Os.fsync(descriptor)
        } finally {
            Os.close(descriptor)
        }
    }

    private fun completed(): ByteArray = JSONObject().put("completed", true).toString().toByteArray()

    private const val SELINUX_XATTR = "security.selinux"
    private const val MODE_MASK = 0x0fff
}

internal class ShizukuFsClientExecutor {
    suspend fun execute(
        candidate: ShizukuSessionLease,
        request: AndroidExecutionRequest,
    ): AndroidExecutionResult {
        val operation = try {
            ShizukuFsCodec.decode(request.payload)
        } catch (error: RuntimeException) {
            throw ShizukuExecutionException("INVALID_ARGUMENT")
        }
        val needsDescriptor = operation is ShizukuFsOperation.FsyncDescriptor ||
            operation is ShizukuFsOperation.ApplyMetadata
        if (request.descriptors.any { it.role != "shizuku_path" } ||
            request.descriptors.size != if (needsDescriptor) 1 else 0
        ) {
            throw ShizukuExecutionException("INVALID_ARGUMENT")
        }
        val completion = CompletableDeferred<AndroidExecutionResult>()
        val input = request.descriptors.singleOrNull()?.descriptor?.let {
            ParcelFileDescriptor.dup(it.fileDescriptor)
        }
        val callback = object : IShizukuFsCallback.Stub() {
            override fun onComplete(
                callbackExecutionId: String?,
                payload: ByteArray?,
                descriptor: ParcelFileDescriptor?,
                errorCode: String?,
            ) {
                if (completion.isCompleted) {
                    runCatching { descriptor?.close() }
                    return
                }
                if (callbackExecutionId != request.executionId) {
                    runCatching { descriptor?.close() }
                    completion.completeExceptionally(ShizukuExecutionException("STALE_AUTHORITY"))
                    return
                }
                if (!errorCode.isNullOrEmpty()) {
                    runCatching { descriptor?.close() }
                    completion.completeExceptionally(ShizukuExecutionException(errorCode))
                    return
                }
                if (payload == null) {
                    runCatching { descriptor?.close() }
                    completion.completeExceptionally(ShizukuExecutionException("IO_ERROR"))
                    return
                }
                val result = AndroidExecutionResult(
                    payload,
                    descriptor?.let { listOf(RoleDescriptor("shizuku_path", it)) }.orEmpty(),
                )
                if (!completion.complete(result)) {
                    result.descriptors.forEach { runCatching { it.descriptor.close() } }
                }
            }
        }
        try {
            candidate.requireCurrent(request)
            candidate.remote.executeFs(
                candidate.token,
                request.executionId,
                request.payload,
                input,
                callback,
            )
        } catch (error: Throwable) {
            completion.completeExceptionally(error)
        } finally {
            runCatching { input?.close() }
        }
        val result = try {
            candidate.awaitCompletion(completion)
        } catch (cancelled: CancellationException) {
            completion.cancel()
            throw cancelled
        } catch (error: Throwable) {
            candidate.requireCurrent(request)
            throw error
        }
        try {
            candidate.requireCurrent(request)
        } catch (error: Throwable) {
            result.descriptors.forEach { runCatching { it.descriptor.close() } }
            throw error
        }
        return result
    }
}
