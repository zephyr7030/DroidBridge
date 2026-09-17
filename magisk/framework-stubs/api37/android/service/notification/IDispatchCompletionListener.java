package android.service.notification;


// Compile-only declaration; the device framework supplies this class at runtime.
public interface IDispatchCompletionListener extends android.os.IInterface {
    void notifyDispatchComplete(long dispatchToken) throws android.os.RemoteException;

    public static abstract class Stub extends android.os.Binder implements IDispatchCompletionListener {
        public static IDispatchCompletionListener asInterface(android.os.IBinder binder) {
            throw new RuntimeException("Stub!");
        }
    }
}
