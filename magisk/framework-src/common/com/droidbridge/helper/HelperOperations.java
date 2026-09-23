package com.droidbridge.helper;

import android.app.Notification;
import android.app.PendingIntent;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.os.RemoteException;
import android.service.notification.StatusBarNotification;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.Set;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

interface NotificationSink {
    NotificationSink NONE = new NotificationSink() {
        @Override
        public void posted(StatusBarNotification notification) {}

        @Override
        public void removed(StatusBarNotification notification) {}

        @Override
        public void invalidateAll() {}
    };

    void posted(StatusBarNotification notification);

    void removed(StatusBarNotification notification);

    void invalidateAll();
}

interface PlatformBinding {
    void registerListener(NotificationSink sink) throws RemoteException;

    void probeListener() throws RemoteException;

    List<StatusBarNotification> activeNotifications() throws RemoteException;

    void cancel(String key) throws RemoteException;

    void sendAction(PendingIntent intent) throws PendingIntent.CanceledException;
}

final class HelperException extends Exception {
    final String code;

    HelperException(String code) {
        super(code);
        this.code = code;
    }
}

/**
 * The fixed S-MAGISK-005 operations served over the authenticated helper socket. The
 * direct listener keeps one callback generation per notification key; dismiss/action
 * revalidate it under the same lock callbacks use, so a replacement is never targeted.
 */
final class HelperOperations implements NotificationSink {
    private static final int MAX_ACTIVE_NOTIFICATIONS = 256;
    private static final int MAX_NOTIFICATION_KEY_BYTES = 2_048;
    private static final int MAX_TITLE_BYTES = 256;
    private static final int MAX_TEXT_BYTES = 512;
    private static final int MAX_ACTIONS = 32;

    private static final class Entry {
        final long generation;
        final StatusBarNotification notification;

        Entry(long generation, StatusBarNotification notification) {
            this.generation = generation;
            this.notification = notification;
        }
    }

    private final PlatformBinding binding;
    private final Map<String, Entry> notifications = new HashMap<>();
    private boolean listening;
    private long nextGeneration = 1;

    HelperOperations(PlatformBinding binding) {
        this.binding = binding;
    }

    JSONObject handle(JSONObject request) {
        try {
            String operation = request.getString("operation");
            switch (operation) {
                case "probe_launch":
                    requireKeys(request);
                    FrameworkServices.probeLaunch();
                    return success(new JSONObject());
                case "probe_notifications":
                    requireKeys(request);
                    binding.probeListener();
                    return success(new JSONObject());
                case "launch_package":
                    requireKeys(request, "package_name");
                    FrameworkServices.launchPackage(packageName(request, "package_name"));
                    return completed();
                case "launch_component":
                    requireKeys(request, "package_name", "class_name");
                    FrameworkServices.startActivity(new Intent(Intent.ACTION_MAIN)
                        .setClassName(packageName(request, "package_name"), className(request)));
                    return completed();
                case "start_view_intent":
                    requireKeys(request, "data_uri", "package_name");
                    return startView(request);
                case "start_explicit_activity":
                    requireKeys(request, "package_name", "class_name", "action", "data_uri", "extras");
                    return startExplicit(request);
                case "notification_snapshot":
                    requireKeys(request);
                    return snapshot();
                case "notification_dismiss":
                    requireKeys(request, "key", "generation");
                    return dismiss(request.getString("key"), request.getLong("generation"));
                case "notification_invoke":
                    requireKeys(request, "key", "generation", "action_index");
                    return invoke(
                        request.getString("key"),
                        request.getLong("generation"),
                        request.getInt("action_index"));
                default:
                    return failure("INVALID_ARGUMENT");
            }
        } catch (HelperException error) {
            return failure(error.code);
        } catch (SecurityException error) {
            return failure("PERMISSION_DENIED");
        } catch (JSONException | IllegalArgumentException error) {
            return failure("INVALID_ARGUMENT");
        } catch (RemoteException error) {
            return failure("CAPABILITY_UNAVAILABLE");
        } catch (Exception error) {
            return failure("IO_ERROR");
        }
    }

    private JSONObject startView(JSONObject request) throws Exception {
        Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(uri(request, "data_uri")));
        if (request.has("package_name")) intent.setPackage(packageName(request, "package_name"));
        FrameworkServices.startActivity(intent);
        return completed();
    }

    private JSONObject startExplicit(JSONObject request) throws Exception {
        Intent intent = new Intent(request.has("action") ? request.getString("action") : Intent.ACTION_MAIN)
            .setClassName(packageName(request, "package_name"), className(request));
        if (request.has("data_uri")) intent.setData(Uri.parse(uri(request, "data_uri")));
        if (request.has("extras")) {
            JSONObject extras = request.getJSONObject("extras");
            if (extras.length() > 32) throw new HelperException("INVALID_ARGUMENT");
            Bundle bundle = new Bundle();
            Iterator<String> keys = extras.keys();
            while (keys.hasNext()) {
                String key = keys.next();
                int keyBytes = key.getBytes(StandardCharsets.UTF_8).length;
                if (keyBytes < 1 || keyBytes > 128) throw new HelperException("INVALID_ARGUMENT");
                Object value = extras.get(key);
                if (value instanceof Boolean) {
                    bundle.putBoolean(key, (Boolean) value);
                } else if (value instanceof Integer || value instanceof Long) {
                    bundle.putLong(key, ((Number) value).longValue());
                } else if (value instanceof String
                    && ((String) value).getBytes(StandardCharsets.UTF_8).length <= 4_096) {
                    bundle.putString(key, (String) value);
                } else {
                    throw new HelperException("INVALID_ARGUMENT");
                }
            }
            intent.putExtras(bundle);
        }
        FrameworkServices.startActivity(intent);
        return completed();
    }

    private synchronized void ensureListening() throws RemoteException {
        if (listening) return;
        binding.registerListener(this);
        listening = true;
        List<StatusBarNotification> active = new ArrayList<>(binding.activeNotifications());
        active.sort((left, right) -> {
            int posted = Long.compare(right.getPostTime(), left.getPostTime());
            return posted != 0 ? posted : left.getKey().compareTo(right.getKey());
        });
        for (StatusBarNotification notification : active) {
            if (notifications.size() >= MAX_ACTIVE_NOTIFICATIONS) break;
            if (admitted(notification) && !notifications.containsKey(notification.getKey())) {
                notifications.put(notification.getKey(), new Entry(allocateGeneration(), notification));
            }
        }
    }

    @Override
    public synchronized void posted(StatusBarNotification notification) {
        if (!admitted(notification)) return;
        notifications.put(notification.getKey(), new Entry(allocateGeneration(), notification));
        if (notifications.size() > MAX_ACTIVE_NOTIFICATIONS) evictOldest();
    }

    @Override
    public synchronized void removed(StatusBarNotification notification) {
        if (notification != null) notifications.remove(notification.getKey());
    }

    @Override
    public synchronized void invalidateAll() {
        notifications.clear();
    }

    private synchronized JSONObject snapshot() throws Exception {
        ensureListening();
        JSONArray entries = new JSONArray();
        List<Map.Entry<String, Entry>> ordered = new ArrayList<>(notifications.entrySet());
        ordered.sort((left, right) -> {
            int posted = Long.compare(
                right.getValue().notification.getPostTime(),
                left.getValue().notification.getPostTime());
            return posted != 0 ? posted : left.getKey().compareTo(right.getKey());
        });
        for (Map.Entry<String, Entry> item : ordered) {
            StatusBarNotification notification = item.getValue().notification;
            Notification platform = notification.getNotification();
            Bundle extras = platform.extras;
            JSONObject entry = new JSONObject()
                .put("key", item.getKey())
                .put("generation", item.getValue().generation)
                .put("package_name", notification.getPackageName());
            if (notification.getPostTime() > 0) entry.put("posted_at_ms", notification.getPostTime());
            CharSequence title = extras == null ? null : extras.getCharSequence(Notification.EXTRA_TITLE);
            CharSequence text = extras == null ? null : extras.getCharSequence(Notification.EXTRA_TEXT);
            if (title != null) entry.put("title", boundedContent(title, MAX_TITLE_BYTES));
            if (text != null) entry.put("text", boundedContent(text, MAX_TEXT_BYTES));
            JSONArray actions = new JSONArray();
            entry.put("action_count", platform.actions == null ? 0 : platform.actions.length);
            if (platform.actions != null) {
                int count = Math.min(platform.actions.length, MAX_ACTIONS + 1);
                for (int index = 0; index < count; index++) {
                    Notification.Action action = platform.actions[index];
                    JSONObject actionFact = new JSONObject()
                        .put("requires_remote_input",
                            action.getRemoteInputs() != null && action.getRemoteInputs().length > 0);
                    if (action.title != null) {
                        actionFact.put("title", boundedContent(action.title, MAX_TEXT_BYTES));
                    }
                    actions.put(actionFact);
                }
            }
            entries.put(entry.put("actions", actions));
        }
        return success(new JSONObject().put("notifications", entries));
    }

    private synchronized JSONObject dismiss(String key, long generation) throws Exception {
        current(key, generation);
        binding.cancel(key);
        return completed();
    }

    private synchronized JSONObject invoke(String key, long generation, int index) throws Exception {
        StatusBarNotification notification = current(key, generation);
        Notification.Action[] actions = notification.getNotification().actions;
        if (index < 0 || index > 31 || actions == null || index >= actions.length) {
            throw new HelperException("INVALID_ARGUMENT");
        }
        Notification.Action action = actions[index];
        if (action.getRemoteInputs() != null && action.getRemoteInputs().length > 0) {
            throw new HelperException("UNSUPPORTED");
        }
        if (action.actionIntent == null) throw new HelperException("UNSUPPORTED");
        try {
            binding.sendAction(action.actionIntent);
        } catch (PendingIntent.CanceledException cancelled) {
            throw new HelperException("EXECUTION_FAILED");
        }
        return completed();
    }

    private StatusBarNotification current(String key, long generation) throws HelperException {
        Entry entry = listening ? notifications.get(key) : null;
        if (entry == null || entry.generation != generation) {
            throw new HelperException("STALE_REFERENCE");
        }
        return entry.notification;
    }

    private static String boundedContent(CharSequence value, int maxBytes) {
        String text = value.toString();
        if (text.getBytes(StandardCharsets.UTF_8).length <= maxBytes) return text;
        int index = 0;
        int bytes = 0;
        while (index < text.length()) {
            int codePoint = text.codePointAt(index);
            int encoded = codePoint <= 0x7f ? 1 : codePoint <= 0x7ff ? 2 : codePoint <= 0xffff ? 3 : 4;
            if (bytes + encoded > maxBytes) break;
            bytes += encoded;
            index += Character.charCount(codePoint);
        }
        return text.substring(0, index);
    }

    private static boolean admitted(StatusBarNotification notification) {
        if (notification == null || notification.getKey() == null || notification.getPackageName() == null) {
            return false;
        }
        int keyBytes = notification.getKey().getBytes(StandardCharsets.UTF_8).length;
        int packageBytes = notification.getPackageName().getBytes(StandardCharsets.UTF_8).length;
        return keyBytes > 0 && keyBytes <= MAX_NOTIFICATION_KEY_BYTES
            && packageBytes > 0 && packageBytes <= 255;
    }

    private long allocateGeneration() {
        if (nextGeneration == Long.MAX_VALUE) throw new IllegalStateException("notification generation exhausted");
        return nextGeneration++;
    }

    private void evictOldest() {
        String oldestKey = null;
        long oldestPostTime = Long.MAX_VALUE;
        for (Map.Entry<String, Entry> item : notifications.entrySet()) {
            long postTime = item.getValue().notification.getPostTime();
            if (oldestKey == null || postTime < oldestPostTime
                || (postTime == oldestPostTime && item.getKey().compareTo(oldestKey) > 0)) {
                oldestKey = item.getKey();
                oldestPostTime = postTime;
            }
        }
        if (oldestKey != null) notifications.remove(oldestKey);
    }

    private static void requireKeys(JSONObject request, String... optional) throws HelperException {
        Set<String> allowed = new HashSet<>(Arrays.asList(optional));
        allowed.add("operation");
        Iterator<String> keys = request.keys();
        while (keys.hasNext()) {
            if (!allowed.contains(keys.next())) throw new HelperException("INVALID_ARGUMENT");
        }
    }

    private static String packageName(JSONObject request, String key) throws Exception {
        return bounded(request.getString(key), 255);
    }

    private static String className(JSONObject request) throws Exception {
        return bounded(request.getString("class_name"), 512);
    }

    private static String uri(JSONObject request, String key) throws Exception {
        return bounded(request.getString(key), 4_096);
    }

    private static String bounded(String value, int maxBytes) throws HelperException {
        byte[] bytes = value.getBytes(StandardCharsets.UTF_8);
        if (value.indexOf(' ') >= 0 || bytes.length > maxBytes) {
            throw new HelperException("INVALID_ARGUMENT");
        }
        return value;
    }

    private static JSONObject completed() throws JSONException {
        return success(new JSONObject().put("completed", true));
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
