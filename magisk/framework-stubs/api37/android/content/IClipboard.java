package android.content;


// Compile-only declaration; the device framework supplies this class at runtime.
public interface IClipboard extends android.os.IInterface {
    void setPrimaryClip(android.content.ClipData clip, String callingPackage, String attributionTag, int userId, int deviceId) throws android.os.RemoteException;
    void clearPrimaryClip(String callingPackage, String attributionTag, int userId, int deviceId) throws android.os.RemoteException;
    android.content.ClipData getPrimaryClip(String pkg, String attributionTag, int userId, int deviceId) throws android.os.RemoteException;

    public static abstract class Stub extends android.os.Binder implements IClipboard {
        public static IClipboard asInterface(android.os.IBinder binder) {
            throw new RuntimeException("Stub!");
        }
    }
}
