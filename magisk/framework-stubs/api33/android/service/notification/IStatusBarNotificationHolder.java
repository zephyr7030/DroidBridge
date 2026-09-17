package android.service.notification;


// Compile-only declaration; the device framework supplies this class at runtime.
public interface IStatusBarNotificationHolder extends android.os.IInterface {
    StatusBarNotification get() throws android.os.RemoteException;

    public static abstract class Stub extends android.os.Binder implements IStatusBarNotificationHolder {
        public static IStatusBarNotificationHolder asInterface(android.os.IBinder binder) {
            throw new RuntimeException("Stub!");
        }
    }
}
