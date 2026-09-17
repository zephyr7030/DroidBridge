package android.os;

        // Compile-only declaration; the device framework supplies this class at runtime.
public final class ServiceManager {
            public static IBinder getService(String name) {
                throw new RuntimeException("Stub!");
            }
        }
