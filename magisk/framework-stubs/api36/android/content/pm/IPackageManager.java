package android.content.pm;


// Compile-only declaration; the device framework supplies this class at runtime.
public interface IPackageManager extends android.os.IInterface {
    int getPackageUid(String packageName, long flags, int userId) throws android.os.RemoteException;
    int checkPermission(String permName, String pkgName, int userId) throws android.os.RemoteException;
    ParceledListSlice queryIntentActivities(android.content.Intent intent, String resolvedType, long flags, int userId) throws android.os.RemoteException;

    public static abstract class Stub extends android.os.Binder implements IPackageManager {
        public static IPackageManager asInterface(android.os.IBinder binder) {
            throw new RuntimeException("Stub!");
        }
    }
}
