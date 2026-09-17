package com.droidbridge.helper;

import android.app.ActivityOptions;
import android.app.INotificationManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationChannelGroup;
import android.app.PendingIntent;
import android.content.ClipData;
import android.content.ComponentName;
import android.content.IClipboard;
import android.content.pm.ParceledListSlice;
import android.os.Bundle;
import android.os.IBinder;
import android.os.RemoteException;
import android.os.ServiceManager;
import android.os.UserHandle;
import android.service.notification.INotificationListener;
import android.service.notification.NotificationRankingUpdate;
import android.service.notification.NotificationStats;
import android.service.notification.StatusBarNotification;
import java.util.Collections;
import java.util.List;
import android.service.notification.IStatusBarNotificationHolder;

/** The API33 Binder declarations used by the fixed S-MAGISK-005 operations. */
final class ApiBinding implements PlatformBinding {
    private static final String SHELL_PACKAGE = "com.android.shell";
    private static final ComponentName LISTENER_COMPONENT =
        new ComponentName(SHELL_PACKAGE, DroidBridgeFrameworkHelper.class.getName());

    private final INotificationManager notifications =
        INotificationManager.Stub.asInterface(ServiceManager.getService("notification"));
    private Listener listener;

    @Override
    public synchronized void registerListener(NotificationSink sink) throws RemoteException {
        Listener candidate = new Listener(sink);
        requireNotifications().registerListener(candidate, LISTENER_COMPONENT, 0);
        listener = candidate;
    }

    @Override
    public void probeListener() throws RemoteException {
        Listener probe = new Listener(NotificationSink.NONE);
        INotificationManager manager = requireNotifications();
        manager.registerListener(probe, LISTENER_COMPONENT, 0);
        manager.unregisterListener(probe, 0);
    }

    @Override
    @SuppressWarnings("unchecked")
    public synchronized List<StatusBarNotification> activeNotifications() throws RemoteException {
        ParceledListSlice slice = requireNotifications().getActiveNotificationsFromListener(listener, null, 0);
        return slice == null ? Collections.emptyList() : (List<StatusBarNotification>) slice.getList();
    }

    @Override
    public synchronized void cancel(String key) throws RemoteException {
        requireNotifications().cancelNotificationsFromListener(listener, new String[] {key});
    }

    @Override
    public void sendAction(PendingIntent intent) throws PendingIntent.CanceledException {
        intent.send();
    }

    static ClipData readClip(IBinder service) throws RemoteException {
        return clipboard(service).getPrimaryClip(SHELL_PACKAGE, 0);
    }

    static void writeClip(IBinder service, ClipData clip) throws RemoteException {
        clipboard(service).setPrimaryClip(clip, SHELL_PACKAGE, 0);
    }

    static void clearClip(IBinder service) throws RemoteException {
        clipboard(service).clearPrimaryClip(SHELL_PACKAGE, 0);
    }

    private static IClipboard clipboard(IBinder service) throws RemoteException {
        IClipboard clipboard = IClipboard.Stub.asInterface(service);
        if (clipboard == null) throw new RemoteException("clipboard service unavailable");
        return clipboard;
    }

    private INotificationManager requireNotifications() throws RemoteException {
        if (notifications == null) throw new RemoteException("notification service unavailable");
        return notifications;
    }

    private static final class Listener extends INotificationListener.Stub {
        private final NotificationSink sink;

        Listener(NotificationSink sink) {
            this.sink = sink;
        }

        @Override
        public void onListenerConnected(NotificationRankingUpdate update) {

        }

        @Override
        public void onNotificationPosted(IStatusBarNotificationHolder notificationHolder, NotificationRankingUpdate update) {
            sink.posted(fetch(notificationHolder));
        }

        @Override
        public void onStatusBarIconsBehaviorChanged(boolean hideSilentStatusIcons) {

        }

        @Override
        public void onNotificationRemoved(IStatusBarNotificationHolder notificationHolder, NotificationRankingUpdate update, NotificationStats stats, int reason) {
            sink.removed(fetch(notificationHolder));
        }

        @Override
        public void onNotificationRankingUpdate(NotificationRankingUpdate update) {

        }

        @Override
        public void onListenerHintsChanged(int hints) {

        }

        @Override
        public void onInterruptionFilterChanged(int interruptionFilter) {

        }

        @Override
        public void onNotificationChannelModification(String pkgName, UserHandle user, NotificationChannel channel, int modificationType) {

        }

        @Override
        public void onNotificationChannelGroupModification(String pkgName, UserHandle user, NotificationChannelGroup group, int modificationType) {

        }

        @Override
        public void onNotificationEnqueuedWithChannel(IStatusBarNotificationHolder notificationHolder, NotificationChannel channel, NotificationRankingUpdate update) {

        }

        @Override
        public void onNotificationSnoozedUntilContext(IStatusBarNotificationHolder notificationHolder, String snoozeCriterionId) {

        }

        @Override
        public void onNotificationsSeen(List<String> keys) {

        }

        @Override
        public void onPanelRevealed(int items) {

        }

        @Override
        public void onPanelHidden() {

        }

        @Override
        public void onNotificationVisibilityChanged(String key, boolean isVisible) {

        }

        @Override
        public void onNotificationExpansionChanged(String key, boolean userAction, boolean expanded) {

        }

        @Override
        public void onNotificationDirectReply(String key) {

        }

        @Override
        public void onSuggestedReplySent(String key, CharSequence reply, int source) {

        }

        @Override
        public void onActionClicked(String key, Notification.Action action, int source) {

        }

        @Override
        public void onNotificationClicked(String key) {

        }

        @Override
        public void onAllowedAdjustmentsChanged() {

        }

        @Override
        public void onNotificationFeedbackReceived(String key, NotificationRankingUpdate update, Bundle feedback) {

        }

        private StatusBarNotification fetch(IStatusBarNotificationHolder holder) {
            try {
                return holder.get();
            } catch (RemoteException | RuntimeException error) {
                sink.invalidateAll();
                return null;
            }
        }
    }
}
