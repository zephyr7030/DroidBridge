package com.droidbridge.helper;

import android.app.IActivityTaskManager;
import android.app.IApplicationThread;
import android.app.ProfilerInfo;
import android.content.Intent;
import android.content.pm.IPackageManager;
import android.content.pm.ParceledListSlice;
import android.content.pm.ResolveInfo;
import android.os.Bundle;
import android.os.IBinder;
import android.os.RemoteException;
import android.os.ServiceManager;
import java.util.List;

/** User-0 activity-task/package service calls shared by every API helper. */
final class FrameworkServices {
    private static final String SHELL_PACKAGE = "com.android.shell";
    private static final int START_SUCCESS_MAX = 3;
    private static final int START_INTENT_NOT_RESOLVED = -1;
    private static final int START_CLASS_NOT_FOUND = -2;
    private static final int START_PERMISSION_DENIED = -4;

    private FrameworkServices() {}

    /**
     * Resolves the service handles and the fixed launch transaction declarations for this
     * SDK without starting an Activity.
     */
    static void probeLaunch() throws HelperException {
        activityTasks();
        packages();
        try {
            IActivityTaskManager.class.getMethod(
                "startActivityAsUser",
                IApplicationThread.class,
                String.class,
                String.class,
                Intent.class,
                String.class,
                IBinder.class,
                String.class,
                int.class,
                int.class,
                ProfilerInfo.class,
                Bundle.class,
                int.class);
            IPackageManager.class.getMethod(
                "queryIntentActivities", Intent.class, String.class, long.class, int.class);
        } catch (NoSuchMethodException | LinkageError error) {
            throw new HelperException("CAPABILITY_UNAVAILABLE");
        }
    }

    /** Resolves the package front door the way PackageManager.getLaunchIntentForPackage does. */
    static void launchPackage(String packageName) throws HelperException, RemoteException {
        Intent resolve = new Intent(Intent.ACTION_MAIN)
            .addCategory(Intent.CATEGORY_INFO)
            .setPackage(packageName);
        ResolveInfo target = first(resolve);
        if (target == null) {
            resolve.removeCategory(Intent.CATEGORY_INFO);
            resolve.addCategory(Intent.CATEGORY_LAUNCHER);
            target = first(resolve);
        }
        if (target == null || target.activityInfo == null) throw new HelperException("NOT_FOUND");
        startActivity(new Intent(resolve)
            .setClassName(target.activityInfo.packageName, target.activityInfo.name));
    }

    static void startActivity(Intent intent) throws HelperException, RemoteException {
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        int result = activityTasks().startActivityAsUser(
            null, SHELL_PACKAGE, null, intent, null, null, null, 0, 0, null, null, 0);
        if (result >= 0 && result <= START_SUCCESS_MAX) return;
        if (result == START_INTENT_NOT_RESOLVED || result == START_CLASS_NOT_FOUND) {
            throw new HelperException("NOT_FOUND");
        }
        if (result == START_PERMISSION_DENIED) throw new HelperException("PERMISSION_DENIED");
        throw new HelperException("EXECUTION_FAILED");
    }

    @SuppressWarnings("unchecked")
    private static ResolveInfo first(Intent intent) throws HelperException, RemoteException {
        ParceledListSlice slice = packages().queryIntentActivities(intent, null, 0L, 0);
        if (slice == null) return null;
        List<ResolveInfo> matches = (List<ResolveInfo>) slice.getList();
        return matches == null || matches.isEmpty() ? null : matches.get(0);
    }

    private static IActivityTaskManager activityTasks() throws HelperException {
        IActivityTaskManager service =
            IActivityTaskManager.Stub.asInterface(ServiceManager.getService("activity_task"));
        if (service == null) throw new HelperException("CAPABILITY_UNAVAILABLE");
        return service;
    }

    private static IPackageManager packages() throws HelperException {
        IPackageManager service = IPackageManager.Stub.asInterface(ServiceManager.getService("package"));
        if (service == null) throw new HelperException("CAPABILITY_UNAVAILABLE");
        return service;
    }
}
