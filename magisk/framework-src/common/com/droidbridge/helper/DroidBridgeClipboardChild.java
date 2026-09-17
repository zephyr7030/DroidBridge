package com.droidbridge.helper;

import android.content.ClipData;
import android.content.pm.IPackageManager;
import android.content.pm.PackageManager;
import android.os.IBinder;
import android.os.PersistableBundle;
import android.os.RemoteException;
import android.os.ServiceManager;
import android.system.Os;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import org.json.JSONException;
import org.json.JSONObject;

/**
 * The short-lived S-MAGISK-005 clipboard child. The native launcher has already set
 * UID/GID 2000; this process proves that identity and calls the clipboard service as
 * `com.android.shell` for exactly one operation.
 */
public final class DroidBridgeClipboardChild {
    private static final String SHELL_PACKAGE = "com.android.shell";
    private static final int SHELL_UID = 2_000;
    private static final int MAX_INPUT_BYTES = 262_144;
    private static final int MAX_TEXT_BYTES = 65_536;

    private DroidBridgeClipboardChild() {}

    public static void main(String[] arguments) {
        JSONObject response;
        try {
            response = run(arguments);
        } catch (HelperException error) {
            response = failure(error.code);
        } catch (SecurityException error) {
            response = failure("PERMISSION_DENIED");
        } catch (JSONException | IllegalArgumentException | IOException error) {
            response = failure("INVALID_ARGUMENT");
        } catch (RemoteException error) {
            response = failure("CAPABILITY_UNAVAILABLE");
        } catch (Exception error) {
            response = failure("IO_ERROR");
        }
        PrintStream output = System.out;
        output.print(response);
        output.flush();
    }

    private static JSONObject run(String[] arguments) throws Exception {
        if (arguments.length != 1) throw new HelperException("INVALID_ARGUMENT");
        JSONObject input = new JSONObject(readInput(System.in));
        if (Os.getuid() != SHELL_UID || Os.getgid() != SHELL_UID) {
            throw new HelperException("PERMISSION_DENIED");
        }
        IPackageManager packages = IPackageManager.Stub.asInterface(ServiceManager.getService("package"));
        if (packages == null
            || packages.getPackageUid(SHELL_PACKAGE, 0L, 0) != SHELL_UID
            || packages.checkPermission(
                "android.permission.READ_CLIPBOARD_IN_BACKGROUND", SHELL_PACKAGE, 0)
                != PackageManager.PERMISSION_GRANTED) {
            throw new HelperException("PERMISSION_DENIED");
        }
        IBinder clipboard = ServiceManager.getService("clipboard");
        if (clipboard == null) throw new HelperException("CAPABILITY_UNAVAILABLE");
        switch (arguments[0]) {
            case "probe":
                requireEmpty(input);
                return success(new JSONObject());
            case "read": {
                requireEmpty(input);
                ClipData clip = ApiBinding.readClip(clipboard);
                JSONObject result = new JSONObject();
                if (clip != null && clip.getItemCount() > 0 && clip.getItemAt(0).getText() != null) {
                    result.put("text", clip.getItemAt(0).getText().toString());
                }
                return success(result);
            }
            case "write": {
                boolean sensitive = input.has("sensitive");
                if (input.length() != (sensitive ? 2 : 1)) throw new HelperException("INVALID_ARGUMENT");
                String text = input.getString("text");
                if (text.getBytes(StandardCharsets.UTF_8).length > MAX_TEXT_BYTES) {
                    throw new HelperException("INVALID_ARGUMENT");
                }
                ClipData clip = ClipData.newPlainText("", text);
                if (sensitive && input.getBoolean("sensitive")) {
                    // ClipDescription.EXTRA_IS_SENSITIVE: keyboards keep the clip out of previews and history.
                    PersistableBundle extras = new PersistableBundle();
                    extras.putBoolean("android.content.extra.IS_SENSITIVE", true);
                    clip.getDescription().setExtras(extras);
                }
                ApiBinding.writeClip(clipboard, clip);
                return success(new JSONObject().put("completed", true));
            }
            case "clear":
                requireEmpty(input);
                ApiBinding.clearClip(clipboard);
                return success(new JSONObject().put("completed", true));
            default:
                throw new HelperException("INVALID_ARGUMENT");
        }
    }

    private static String readInput(InputStream input) throws IOException, HelperException {
        ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        byte[] buffer = new byte[8_192];
        int read;
        while ((read = input.read(buffer)) != -1) {
            if (bytes.size() + read > MAX_INPUT_BYTES) throw new HelperException("INVALID_ARGUMENT");
            bytes.write(buffer, 0, read);
        }
        return new String(bytes.toByteArray(), StandardCharsets.UTF_8);
    }

    private static void requireEmpty(JSONObject input) throws HelperException {
        if (input.length() != 0) throw new HelperException("INVALID_ARGUMENT");
    }

    private static JSONObject success(JSONObject result) throws JSONException {
        return new JSONObject().put("ok", true).put("result", result);
    }

    private static JSONObject failure(String code) {
        try {
            return new JSONObject().put("ok", false).put("code", code);
        } catch (JSONException impossible) {
            throw new IllegalStateException(impossible);
        }
    }
}
