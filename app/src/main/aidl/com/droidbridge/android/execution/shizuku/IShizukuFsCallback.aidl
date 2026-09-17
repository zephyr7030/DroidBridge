package com.droidbridge.android.execution.shizuku;

import android.os.ParcelFileDescriptor;

interface IShizukuFsCallback {
    void onComplete(
        String executionId,
        in byte[] payload,
        in ParcelFileDescriptor descriptor,
        String errorCode
    );
}
