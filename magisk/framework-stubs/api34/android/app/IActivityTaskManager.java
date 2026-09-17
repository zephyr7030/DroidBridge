package android.app;


// Compile-only declaration; the device framework supplies this class at runtime.
public interface IActivityTaskManager extends android.os.IInterface {
    int startActivityAsUser(IApplicationThread caller, String callingPackage, String callingFeatureId, android.content.Intent intent, String resolvedType, android.os.IBinder resultTo, String resultWho, int requestCode, int flags, ProfilerInfo profilerInfo, android.os.Bundle options, int userId) throws android.os.RemoteException;

    public static abstract class Stub extends android.os.Binder implements IActivityTaskManager {
        public static IActivityTaskManager asInterface(android.os.IBinder binder) {
            throw new RuntimeException("Stub!");
        }
    }
}
