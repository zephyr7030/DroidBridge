package com.droidbridge.android.runtimehost;

import com.droidbridge.android.runtimehost.IRuntimeCallback;
import com.droidbridge.android.runtimehost.IRuntimeEventCallback;

interface IDroidBridgeRuntime {
    void submit(in byte[] envelope, IRuntimeCallback callback);
    void cancelRequest(String requestId);
    void subscribe(IRuntimeEventCallback callback);
    void unsubscribe(IRuntimeEventCallback callback);
    void requestCapabilityRecheck();
    boolean requestShizukuAuthorization();
    String getMcpSettings();
    String setMcpEnabled(boolean enabled);
    String rotateMcpToken();
    String revealMcpToken();
    String getTunnelSettings();
    String configureTunnel(String tunnelId, String apiKey);
    String setTunnelEnabled(boolean enabled);
    String clearTunnel();
    String getMaintenanceState();
    String getDiagnosticsSnapshot();
    String resetRuntimeData();
    String resetRuntimeHostToApk();
    String getUpdateMaintenance();
    String beginProductUpdate(in byte[] manifest, in byte[] signature);
    String beginModuleRepair(in byte[] manifest, in byte[] signature);
    String installUpdateApk(String updateId);
    String installUpdateModule(String updateId);
    String cancelUpdate(String updateId);
    String continueWithoutModule(String updateId);
    int getStrandedExecutions();
    String clearStrandedExecutions();
}
