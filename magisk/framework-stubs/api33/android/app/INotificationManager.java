package android.app;


// Compile-only declaration; the device framework supplies this class at runtime.
public interface INotificationManager extends android.os.IInterface {
    void registerListener(android.service.notification.INotificationListener listener, android.content.ComponentName component, int userid) throws android.os.RemoteException;
    void unregisterListener(android.service.notification.INotificationListener listener, int userid) throws android.os.RemoteException;
    void cancelNotificationsFromListener(android.service.notification.INotificationListener token, String[] keys) throws android.os.RemoteException;
    android.content.pm.ParceledListSlice getActiveNotificationsFromListener(android.service.notification.INotificationListener token, String[] keys, int trim) throws android.os.RemoteException;

    public static abstract class Stub extends android.os.Binder implements INotificationManager {
        public static INotificationManager asInterface(android.os.IBinder binder) {
            throw new RuntimeException("Stub!");
        }
    }
}
