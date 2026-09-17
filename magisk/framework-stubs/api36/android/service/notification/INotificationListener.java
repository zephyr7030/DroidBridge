package android.service.notification;


// Compile-only declaration; the device framework supplies this class at runtime.
public interface INotificationListener extends android.os.IInterface {
    void onListenerConnected(android.service.notification.NotificationRankingUpdate update) throws android.os.RemoteException;
    void onNotificationPosted(android.service.notification.IStatusBarNotificationHolder notificationHolder, android.service.notification.NotificationRankingUpdate update) throws android.os.RemoteException;
    void onNotificationPostedFull(android.service.notification.StatusBarNotification sbn, android.service.notification.NotificationRankingUpdate update) throws android.os.RemoteException;
    void onStatusBarIconsBehaviorChanged(boolean hideSilentStatusIcons) throws android.os.RemoteException;
    void onNotificationRemoved(android.service.notification.IStatusBarNotificationHolder notificationHolder, android.service.notification.NotificationRankingUpdate update, android.service.notification.NotificationStats stats, int reason) throws android.os.RemoteException;
    void onNotificationRemovedFull(android.service.notification.StatusBarNotification sbn, android.service.notification.NotificationRankingUpdate update, android.service.notification.NotificationStats stats, int reason) throws android.os.RemoteException;
    void onNotificationRankingUpdate(android.service.notification.NotificationRankingUpdate update) throws android.os.RemoteException;
    void onListenerHintsChanged(int hints) throws android.os.RemoteException;
    void onInterruptionFilterChanged(int interruptionFilter) throws android.os.RemoteException;
    void onNotificationChannelModification(String pkgName, android.os.UserHandle user, android.app.NotificationChannel channel, int modificationType) throws android.os.RemoteException;
    void onNotificationChannelGroupModification(String pkgName, android.os.UserHandle user, android.app.NotificationChannelGroup group, int modificationType) throws android.os.RemoteException;
    void onNotificationEnqueuedWithChannel(android.service.notification.IStatusBarNotificationHolder notificationHolder, android.app.NotificationChannel channel, android.service.notification.NotificationRankingUpdate update) throws android.os.RemoteException;
    void onNotificationEnqueuedWithChannelFull(android.service.notification.StatusBarNotification sbn, android.app.NotificationChannel channel, android.service.notification.NotificationRankingUpdate update) throws android.os.RemoteException;
    void onNotificationSnoozedUntilContext(android.service.notification.IStatusBarNotificationHolder notificationHolder, String snoozeCriterionId) throws android.os.RemoteException;
    void onNotificationSnoozedUntilContextFull(android.service.notification.StatusBarNotification sbn, String snoozeCriterionId) throws android.os.RemoteException;
    void onNotificationsSeen(java.util.List<String> keys) throws android.os.RemoteException;
    void onPanelRevealed(int items) throws android.os.RemoteException;
    void onPanelHidden() throws android.os.RemoteException;
    void onNotificationVisibilityChanged(String key, boolean isVisible) throws android.os.RemoteException;
    void onNotificationExpansionChanged(String key, boolean userAction, boolean expanded) throws android.os.RemoteException;
    void onNotificationDirectReply(String key) throws android.os.RemoteException;
    void onSuggestedReplySent(String key, CharSequence reply, int source) throws android.os.RemoteException;
    void onActionClicked(String key, android.app.Notification.Action action, int source) throws android.os.RemoteException;
    void onNotificationClicked(String key) throws android.os.RemoteException;
    void onAllowedAdjustmentsChanged() throws android.os.RemoteException;
    void onNotificationFeedbackReceived(String key, android.service.notification.NotificationRankingUpdate update, android.os.Bundle feedback) throws android.os.RemoteException;

    public static abstract class Stub extends android.os.Binder implements INotificationListener {
        public Stub() {
            throw new RuntimeException("Stub!");
        }

        public android.os.IBinder asBinder() {
            throw new RuntimeException("Stub!");
        }
    }
}
