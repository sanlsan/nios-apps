package ru.nios.apps;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.IBinder;
import android.os.PowerManager;

import com.chaquo.python.PyObject;
import com.chaquo.python.Python;
import com.chaquo.python.android.AndroidPlatform;

import java.util.function.Consumer;

public class RunnerService extends Service {
    static final String ACTION_START = "start";
    static final String ACTION_STOP = "stop";
    private static final String CHANNEL = "tunnel";
    private PowerManager.WakeLock lock;

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        String action = intent == null ? ACTION_STOP : intent.getAction();
        if (ACTION_START.equals(action)) {
            begin(intent);
        } else {
            end();
        }
        return START_NOT_STICKY;
    }

    private void begin(Intent intent) {
        NotificationManager manager = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        manager.createNotificationChannel(new NotificationChannel(CHANNEL, "Сервер", NotificationManager.IMPORTANCE_LOW));
        Intent open = new Intent(this, MainActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent tap = PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE);
        Intent stop = new Intent(this, RunnerService.class).setAction(ACTION_STOP);
        PendingIntent stopTap = PendingIntent.getService(this, 1, stop, PendingIntent.FLAG_IMMUTABLE);
        Notification note = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(R.drawable.ic_stat)
                .setContentTitle("Nios Apps работает")
                .setContentText("Ваш сервер доступен из интернета")
                .setContentIntent(tap)
                .addAction(new Notification.Action.Builder(null, "Остановить", stopTap).build())
                .setOngoing(true)
                .build();
        startForeground(1, note);
        PowerManager power = (PowerManager) getSystemService(Context.POWER_SERVICE);
        if (lock == null) {
            lock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nios:tunnel");
        }
        if (!lock.isHeld()) {
            lock.acquire();
        }
        Bus.running = true;
        Bus.halt = this::halt;
        final String source = intent.getStringExtra("code");
        final String key = intent.getStringExtra("key");
        final String packages = intent.getStringExtra("packages");
        final String port = intent.getStringExtra("port");
        final String server = BuildConfig.DEBUG ? intent.getStringExtra("server") : null;
        final Consumer<String> sink = Bus::emit;
        new Thread(() -> {
            if (!Python.isStarted()) {
                Python.start(new AndroidPlatform(this));
            }
            PyObject module = Python.getInstance().getModule("nios_android");
            module.callAttr("start", getFilesDir().getAbsolutePath(), source, key, packages, port, server, sink);
        }, "nios-start").start();
    }

    private void halt() {
        Bus.running = false;
        Bus.url = "";
        if (lock != null && lock.isHeld()) {
            lock.release();
        }
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    private void end() {
        if (Python.isStarted()) {
            try {
                Python.getInstance().getModule("nios_android").callAttr("stop");
            } catch (Exception ignored) {
            }
        }
        Bus.running = false;
        Bus.url = "";
        if (lock != null && lock.isHeld()) {
            lock.release();
        }
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    @Override
    public void onDestroy() {
        if (lock != null && lock.isHeld()) {
            lock.release();
        }
        super.onDestroy();
    }
}
