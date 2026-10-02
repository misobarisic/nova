package dev.misob.nova;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;

import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

/** Keeps OAuth reception, token exchange and account verification alive.
 * Rust owns the login leases; this service contains no authorization secrets.
 * Separate from downloads so either task can finish without stopping the other.
 */
public class NovaAuthService extends Service {
    private static final String CHANNEL = "nova.signin";
    private static final int NOTIFICATION = 0x54;
    // Five minutes for browser approval plus two bounded 30-second API calls.
    private static final long MAX_LIFETIME_MS = 370_000;
    private static final AtomicInteger nextRequest = new AtomicInteger();
    private static final ConcurrentHashMap<Integer, Start> requests = new ConcurrentHashMap<>();

    private static final class Start {
        final CountDownLatch ready = new CountDownLatch(1);
        volatile boolean success;
    }

    private final Handler handler = new Handler(Looper.getMainLooper());
    private final Runnable expire = () -> stopSelf();
    private PowerManager.WakeLock wakeLock;
    private boolean foreground;

    /** Called from the Rust actor, never the main thread. Do not open the
     * browser until Android has actually promoted the service to foreground.
     */
    public static boolean start(Context context, String title, String text) {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            return false;
        }
        int id = nextRequest.incrementAndGet();
        Start request = new Start();
        requests.put(id, request);
        try {
            Intent intent = new Intent(context, NovaAuthService.class)
                    .putExtra("request", id).putExtra("title", title).putExtra("text", text);
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(intent);
            } else {
                context.startService(intent);
            }
            return request.ready.await(5, TimeUnit.SECONDS) && request.success;
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            return false;
        } catch (RuntimeException error) {
            // JNI callers get a plain failure, never an uncaught platform error.
            return false;
        } finally {
            requests.remove(id);
        }
    }

    public static void stop(Context context) {
        context.stopService(new Intent(context, NovaAuthService.class));
    }

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager manager = getSystemService(NotificationManager.class);
        manager.createNotificationChannel(new NotificationChannel(
                CHANNEL, "Tracker sign-in", NotificationManager.IMPORTANCE_LOW));
        PowerManager power = getSystemService(PowerManager.class);
        if (power != null) {
            wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nova:signin");
            wakeLock.setReferenceCounted(false);
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        Start request = intent == null ? null : requests.get(intent.getIntExtra("request", 0));
        if (request == null) {
            // A timed-out start must not leave an orphan foreground service.
            if (!foreground) {
                stopSelf(startId);
            }
            return START_NOT_STICKY;
        }
        try {
            Notification notification = notification(intent.getStringExtra("title"),
                    intent.getStringExtra("text"));
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
            } else {
                startForeground(NOTIFICATION, notification);
            }
            foreground = true;
            if (wakeLock != null) {
                wakeLock.acquire(MAX_LIFETIME_MS);
            }
            handler.removeCallbacks(expire);
            handler.postDelayed(expire, MAX_LIFETIME_MS);
            request.success = true;
        } catch (RuntimeException error) {
            stopSelf(startId);
        } finally {
            request.ready.countDown();
        }
        // OAuth sessions belong to this process and cannot be reconstructed by
        // restarting an empty Service. Expiry also covers a stalled Rust actor.
        return START_NOT_STICKY;
    }

    @Override
    public void onTimeout(int startId, int foregroundServiceType) {
        stopSelf();
    }

    @Override
    public void onDestroy() {
        handler.removeCallbacks(expire);
        if (wakeLock != null && wakeLock.isHeld()) {
            wakeLock.release();
        }
        stopForeground(STOP_FOREGROUND_REMOVE);
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private Notification notification(String title, String text) {
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        Notification.Builder builder = new Notification.Builder(this, CHANNEL)
                .setContentTitle(title).setContentText(text)
                .setSmallIcon(android.R.drawable.ic_lock_lock)
                .setOngoing(true).setShowWhen(false);
        if (launch != null) {
            launch.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
            builder.setContentIntent(PendingIntent.getActivity(this, 0, launch,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        }
        return builder.build();
    }
}
