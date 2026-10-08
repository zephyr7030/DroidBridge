package com.droidbridge.standalone.runtimehost;

import com.droidbridge.standalone.runtimehost.IRuntimeCallback;
import com.droidbridge.standalone.runtimehost.IRuntimeEventCallback;

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
    String getUpdateMaintenance();
    String beginProductUpdate(in byte[] manifest, in byte[] signature);
    String installUpdateApk(String updateId);
    String cancelUpdate(String updateId);
    int getStrandedExecutions();
    String clearStrandedExecutions();
    boolean getKeepAliveEnabled();
    boolean setKeepAliveEnabled(boolean enabled);
}
