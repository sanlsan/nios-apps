package ru.nios.apps;

import android.Manifest;
import android.annotation.SuppressLint;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.PowerManager;
import android.provider.Settings;
import android.view.ViewGroup;
import android.webkit.JavascriptInterface;
import android.webkit.WebChromeClient;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.function.Consumer;

public class MainActivity extends android.app.Activity {
    private static final String APK_URL = "https://ni-os.ru/appsdev/download/NiosApps.apk";
    private WebView web;
    private SharedPreferences prefs;
    private final Consumer<String> sink = this::toPage;
    private boolean ready;

    @SuppressLint({"SetJavaScriptEnabled", "AddJavascriptInterface"})
    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        prefs = getSharedPreferences("nios", MODE_PRIVATE);
        web = new WebView(this);
        web.setLayoutParams(new ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        setContentView(web);
        WebSettings settings = web.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(true);
        settings.setAllowFileAccess(false);
        settings.setAllowContentAccess(false);
        web.setWebChromeClient(new WebChromeClient());
        web.setWebViewClient(new WebViewClient() {
            @Override
            public void onPageFinished(WebView view, String url) {
                ready = true;
                if (Bus.running && !Bus.url.isEmpty()) {
                    toPage("{\"t\": \"online\", \"url\": " + JSONObject.quote(Bus.url) + "}");
                }
                checkUpdate(false);
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView view, android.webkit.WebResourceRequest request) {
                return true;
            }
        });
        web.addJavascriptInterface(new Native(), "NiosAndroid");
        web.loadDataWithBaseURL("https://nios.local/", page(), "text/html", "utf-8", null);
        if (Build.VERSION.SDK_INT >= 33) {
            requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, 1);
        }
    }

    @Override
    protected void onStart() {
        super.onStart();
        Bus.attach(sink);
    }

    @Override
    protected void onStop() {
        Bus.detach(sink);
        super.onStop();
    }

    @Override
    public void onBackPressed() {
        moveTaskToBack(true);
    }

    private String asset(String name) {
        try (InputStream in = getAssets().open(name)) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buf = new byte[8192];
            int n;
            while ((n = in.read(buf)) > 0) {
                out.write(buf, 0, n);
            }
            return out.toString("UTF-8");
        } catch (Exception e) {
            return "";
        }
    }

    private String page() {
        try {
            JSONObject init = new JSONObject();
            init.put("key", prefs.getString("key", ""));
            init.put("code", prefs.getString("code", ""));
            init.put("packages", prefs.getString("packages", ""));
            init.put("folder", "");
            init.put("port", prefs.getString("port", ""));
            init.put("version", BuildConfig.VERSION_NAME);
            init.put("auto_update", true);
            String shim = "<script>window.__init=" + init.toString().replace("</", "<\\/") + ";"
                    + "window.ipc={postMessage:function(s){NiosAndroid.post(s)}};"
                    + "document.addEventListener('DOMContentLoaded',function(){"
                    + "['folder','port','open-data','auto-update'].forEach(function(id){var e=document.getElementById(id);"
                    + "var s=e&&e.closest('.set');if(s)s.hidden=true;});"
                    + "var p=document.querySelector('#live p');if(p)p.textContent='Отправьте этот адрес кому угодно. Сервер работает, пока приложение запущено: оно остаётся в уведомлениях.';"
                    + "});</script>";
            String html = asset("index.html").replace("/*FONTS*/", asset("fonts.css"));
            return html.replace("</head>", shim + "</head>");
        } catch (Exception e) {
            return "<p>" + e + "</p>";
        }
    }

    private void toPage(String json) {
        if (!ready) {
            return;
        }
        web.evaluateJavascript("window.onNative&&window.onNative(" + json + ")", null);
    }

    private void checkUpdate(boolean manual) {
        new Thread(() -> {
            try {
                HttpURLConnection c = (HttpURLConnection) new URL("https://api.github.com/repos/" + BuildConfig.REPO + "/releases?per_page=10").openConnection();
                c.setConnectTimeout(8000);
                c.setReadTimeout(8000);
                c.setRequestProperty("Accept", "application/vnd.github+json");
                ByteArrayOutputStream out = new ByteArrayOutputStream();
                try (InputStream in = c.getInputStream()) {
                    byte[] buf = new byte[8192];
                    int n;
                    while ((n = in.read(buf)) > 0) {
                        out.write(buf, 0, n);
                    }
                }
                JSONArray list = new JSONArray(out.toString("UTF-8"));
                String best = null;
                for (int pass = 0; pass < 2 && best == null; pass++) {
                    for (int i = 0; i < list.length(); i++) {
                        JSONObject r = list.getJSONObject(i);
                        if (r.optBoolean("draft") || (pass == 0 && r.optBoolean("prerelease"))) {
                            continue;
                        }
                        JSONArray assets = r.optJSONArray("assets");
                        for (int j = 0; assets != null && j < assets.length(); j++) {
                            if ("NiosApps.apk".equals(assets.getJSONObject(j).optString("name"))) {
                                best = r.getString("tag_name");
                                break;
                            }
                        }
                        if (best != null) {
                            break;
                        }
                    }
                }
                final String latest = best;
                runOnUiThread(() -> {
                    if (latest != null && isNewer(latest.replaceFirst("^v", ""), BuildConfig.VERSION_NAME)) {
                        toPage("{\"t\":\"update_ready\",\"version\":" + JSONObject.quote(latest) + "}");
                    } else if (manual) {
                        toPage("{\"t\":\"update_none\",\"manual\":true,\"version\":" + JSONObject.quote(BuildConfig.VERSION_NAME) + "}");
                    }
                });
            } catch (Exception e) {
                if (manual) {
                    runOnUiThread(() -> toPage("{\"t\":\"update_error\",\"manual\":true,\"message\":\"Не удалось проверить обновления.\"}"));
                }
            }
        }, "nios-update").start();
    }

    static boolean isNewer(String candidate, String current) {
        int[] a = numbers(candidate), b = numbers(current);
        for (int i = 0; i < 3; i++) {
            if (a[i] != b[i]) {
                return a[i] > b[i];
            }
        }
        boolean aPre = candidate.contains("-"), bPre = current.contains("-");
        if (aPre != bPre) {
            return bPre;
        }
        return aPre && candidate.compareTo(current) > 0;
    }

    private static int[] numbers(String v) {
        int[] out = new int[3];
        String[] parts = v.split("-")[0].split("\\.");
        for (int i = 0; i < 3 && i < parts.length; i++) {
            try {
                out[i] = Integer.parseInt(parts[i]);
            } catch (NumberFormatException ignored) {
            }
        }
        return out;
    }

    private void askBatteryExemption() {
        PowerManager power = (PowerManager) getSystemService(Context.POWER_SERVICE);
        if (!power.isIgnoringBatteryOptimizations(getPackageName())) {
            try {
                startActivity(new Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:" + getPackageName())));
            } catch (Exception ignored) {
            }
        }
    }

    private final class Native {
        @JavascriptInterface
        public void post(String raw) {
            runOnUiThread(() -> handle(raw));
        }
    }

    private void handle(String raw) {
        try {
            JSONObject m = new JSONObject(raw);
            String cmd = m.optString("cmd");
            switch (cmd) {
                case "save":
                    save(m);
                    break;
                case "start": {
                    save(m);
                    String key = m.optString("key").trim();
                    if (!key.startsWith("nios_app_")) {
                        toPage("{\"t\":\"error\",\"message\":\"Ключ должен начинаться с nios_app_. Скопируйте его из кабинета целиком.\"}");
                        break;
                    }
                    Intent i = new Intent(this, RunnerService.class).setAction(RunnerService.ACTION_START)
                            .putExtra("key", key).putExtra("code", m.optString("code"))
                            .putExtra("packages", m.optString("packages")).putExtra("port", m.optString("port"));
                    if (BuildConfig.DEBUG) {
                        i.putExtra("server", prefs.getString("server", null));
                    }
                    startForegroundService(i);
                    askBatteryExemption();
                    break;
                }
                case "stop":
                    startService(new Intent(this, RunnerService.class).setAction(RunnerService.ACTION_STOP));
                    break;
                case "copy": {
                    ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
                    cm.setPrimaryClip(ClipData.newPlainText("url", m.optString("text")));
                    break;
                }
                case "open": {
                    String url = m.optString("url");
                    if (url.startsWith("https://")) {
                        startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(url)));
                    }
                    break;
                }
                case "check_update":
                    checkUpdate(true);
                    break;
                case "apply_update":
                    startActivity(new Intent(Intent.ACTION_VIEW, Uri.parse(APK_URL)));
                    break;
                default:
                    break;
            }
        } catch (Exception ignored) {
        }
    }

    private void save(JSONObject m) {
        prefs.edit().putString("key", m.optString("key")).putString("code", m.optString("code"))
                .putString("packages", m.optString("packages")).putString("port", m.optString("port")).apply();
    }
}
