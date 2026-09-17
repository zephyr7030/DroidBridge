package com.droidbridge.helper;

import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Build;
import android.os.ParcelFileDescriptor;
import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import org.json.JSONException;
import org.json.JSONObject;

public final class DroidBridgeFrameworkHelper {
    private static final int MAX_FRAME_BYTES = 1_048_576;

    private DroidBridgeFrameworkHelper() {}

    public static void main(String[] arguments) throws Exception {
        if (arguments.length != 3) {
            throw new IllegalArgumentException("expected socket, SDK, and generation");
        }
        int sdkInt = Integer.parseInt(arguments[1]);
        long generation = Long.parseLong(arguments[2]);
        if (sdkInt != Build.VERSION.SDK_INT || sdkInt < 33 || sdkInt > 37 || generation < 1) {
            throw new IllegalStateException("helper identity mismatch");
        }
        int listenerFd = Integer.parseInt(arguments[0]);
        try (
            ParcelFileDescriptor listenerDescriptor = ParcelFileDescriptor.adoptFd(listenerFd);
            LocalServerSocket server = new LocalServerSocket(listenerDescriptor.getFileDescriptor());
            LocalSocket socket = server.accept()
        ) {
            if (socket.getPeerCredentials().getUid() != 0) {
                throw new SecurityException("framework helper peer is not root");
            }
            DataOutputStream output = new DataOutputStream(socket.getOutputStream());
            DataInputStream input = new DataInputStream(socket.getInputStream());
            writeFrame(output, new JSONObject()
                .put("protocol_version", 1)
                .put("sdk_int", sdkInt)
                .put("helper_generation", generation));
            HelperOperations operations = new HelperOperations(new ApiBinding());
            while (true) {
                JSONObject request;
                try {
                    request = readFrame(input);
                } catch (EOFException closed) {
                    return;
                }
                writeFrame(output, operations.handle(request));
            }
        }
    }

    private static JSONObject readFrame(DataInputStream input) throws IOException, JSONException {
        int length = input.readInt();
        if (length <= 0 || length > MAX_FRAME_BYTES) {
            throw new IOException("invalid helper frame length");
        }
        byte[] body = new byte[length];
        input.readFully(body);
        return new JSONObject(new String(body, StandardCharsets.UTF_8));
    }

    private static void writeFrame(DataOutputStream output, JSONObject value) throws IOException {
        byte[] body = value.toString().getBytes(StandardCharsets.UTF_8);
        output.writeInt(body.length);
        output.write(body);
        output.flush();
    }
}
