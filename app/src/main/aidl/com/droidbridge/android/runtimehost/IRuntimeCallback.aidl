package com.droidbridge.android.runtimehost;

oneway interface IRuntimeCallback {
    void onResponse(in byte[] envelope);
}
