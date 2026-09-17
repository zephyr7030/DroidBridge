package com.droidbridge.android.execution.shizuku;

interface IShizukuExecutionCallback {
    void onComplete(String executionId, int exitCode, String errorCode);
}
