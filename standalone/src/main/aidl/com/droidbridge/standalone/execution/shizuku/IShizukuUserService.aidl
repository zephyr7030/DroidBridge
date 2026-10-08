package com.droidbridge.standalone.execution.shizuku;

import android.os.IBinder;
import android.os.ParcelFileDescriptor;
import com.droidbridge.standalone.execution.shizuku.IShizukuExecutionCallback;
import com.droidbridge.standalone.execution.shizuku.IShizukuFsCallback;

interface IShizukuUserService {
    int getUid() = 1;
    void attachClient(IBinder token, String clientId) = 2;
    void detachClient(IBinder token) = 3;
    void executeGuarded(
        IBinder token,
        String executionId,
        String primitive,
        in byte[] payload,
        in ParcelFileDescriptor proofFd,
        in ParcelFileDescriptor stdinFd,
        in ParcelFileDescriptor stdoutFd,
        in ParcelFileDescriptor stderrFd,
        IShizukuExecutionCallback callback
    ) = 4;
    void executeFs(
        IBinder token,
        String executionId,
        in byte[] payload,
        in ParcelFileDescriptor descriptor,
        IShizukuFsCallback callback
    ) = 5;
    boolean cancel(IBinder token, String executionId) = 6;
    void setKeepAlive(IBinder token, boolean enabled) = 7;
    boolean setIdleExemption(IBinder token, boolean exempt) = 8;
    boolean setBackgroundAllowed(IBinder token, boolean allowed) = 9;
    void destroy() = 16777114;
}
