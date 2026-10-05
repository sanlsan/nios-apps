package ru.nios.apps;

import android.os.Handler;
import android.os.Looper;

import java.util.function.Consumer;

final class Bus {
    private static final Handler MAIN = new Handler(Looper.getMainLooper());
    private static Consumer<String> sink;
    static volatile boolean running;
    static volatile String url = "";
    static volatile Runnable halt;

    private Bus() {
    }

    static synchronized void attach(Consumer<String> next) {
        sink = next;
    }

    static synchronized void detach(Consumer<String> current) {
        if (sink == current) {
            sink = null;
        }
    }

    static void emit(String json) {
        if (json.contains("\"t\": \"online\"")) {
            try {
                url = new org.json.JSONObject(json).optString("url");
            } catch (Exception ignored) {
            }
        } else if (json.contains("\"t\": \"stopped\"") || json.contains("\"t\": \"error\"")) {
            running = false;
            url = "";
            Runnable stop = halt;
            if (stop != null) {
                MAIN.post(stop);
            }
        }
        final Consumer<String> target;
        synchronized (Bus.class) {
            target = sink;
        }
        if (target != null) {
            MAIN.post(() -> target.accept(json));
        }
    }
}
