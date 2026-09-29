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
import android.os.IBinder;
import android.os.PowerManager;

import java.lang.ref.WeakReference;

/**
 * Foreground service that keeps the app process (and its transfer threads)
 * alive while a stream download is running, including with the screen off.
 *
 * The actual download work lives in Rust; this service only:
 *  - holds a partial wake lock while it runs,
 *  - shows the ongoing notification Android requires for a foreground
 *    service, and
 *  - is started/stopped/updated from Rust through the static helpers below.
 *
 * `foregroundServiceType="dataSync"` is declared in the manifest. The helper
 * methods are intentionally tiny wrappers so Rust only has to call a static
 * method with the Context (no Android API calls in Rust).
 */
public class NovaBackgroundService extends Service {
    private static final String CHANNEL_ID = "nova.background";
    private static final int NOTIFICATION_ID = 0x51;

    /** Live instance, so {@link #update} can rebuild the notification. */
    private static WeakReference<NovaBackgroundService> instance = new WeakReference<>(null);

    private PowerManager.WakeLock wakeLock;

    /** Start (or bring up) the download foreground service. */
    public static void start(Context context) {
        ensureNotificationPermission(context);
        Intent intent = new Intent(context, NovaBackgroundService.class);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            context.startForegroundService(intent);
        } else {
            context.startService(intent);
        }
    }

    /**
     * API 33+: the foreground-service notification is hidden without this, and a
     * Service cannot request it. Ask when started from the Activity (the only
     * context Rust passes); no-op elsewhere.
     */
    private static void ensureNotificationPermission(Context context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU
                || !(context instanceof android.app.Activity)) {
            return;
        }
        android.app.Activity activity = (android.app.Activity) context;
        if (activity.checkSelfPermission("android.permission.POST_NOTIFICATIONS")
                != android.content.pm.PackageManager.PERMISSION_GRANTED) {
            activity.requestPermissions(
                    new String[] {"android.permission.POST_NOTIFICATIONS"}, 0x53);
        }
    }

    /** Stop the service (no active transfers). Safe to call when not running. */
    public static void stop(Context context) {
        context.stopService(new Intent(context, NovaBackgroundService.class));
    }

    /** Refresh the notification text; no-op when the service is not running. */
    public static void update(Context context, String title, String text) {
        NovaBackgroundService service = instance.get();
        if (service == null) {
            return;
        }
        Context app = service.getApplicationContext();
        NotificationManager manager =
                (NotificationManager) app.getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager != null) {
            manager.notify(NOTIFICATION_ID, service.buildNotification(title, text));
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        instance = new WeakReference<>(this);
        createChannel();
        PowerManager power = (PowerManager) getSystemService(Context.POWER_SERVICE);
        if (power != null) {
            wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nova:downloads");
            wakeLock.setReferenceCounted(false);
            wakeLock.acquire();
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        Notification notification = buildNotification("Downloading", "Preparing…");
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(
                    NOTIFICATION_ID,
                    notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        } else {
            startForeground(NOTIFICATION_ID, notification);
        }
        // Do not restart on its own: a restarted service with no Activity has no
        // transfer threads to keep alive, and downloads resume when the app is
        // next opened (persisted queue + `.part`/Range).
        return START_NOT_STICKY;
    }

    /** Android 15: dataSync foreground services are capped at 6 h/24 h. */
    @Override
    public void onTimeout(int startId, int foregroundServiceType) {
        stopSelf();
    }

    @Override
    public void onDestroy() {
        instance = new WeakReference<>(null);
        if (wakeLock != null && wakeLock.isHeld()) {
            wakeLock.release();
        }
        wakeLock = null;
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private void createChannel() {
        NotificationManager manager =
                (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        if (manager == null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID,
                "Downloads",
                NotificationManager.IMPORTANCE_LOW);
        channel.setDescription("Active stream downloads");
        manager.createNotificationChannel(channel);
    }

    private Notification buildNotification(String title, String text) {
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        PendingIntent contentIntent = null;
        if (launch != null) {
            launch.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP);
            int flags = PendingIntent.FLAG_UPDATE_CURRENT;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                flags |= PendingIntent.FLAG_IMMUTABLE;
            }
            contentIntent = PendingIntent.getActivity(this, 0, launch, flags);
        }
        Notification.Builder builder = new Notification.Builder(this, CHANNEL_ID)
                .setContentTitle(title)
                .setContentText(text)
                .setSmallIcon(android.R.drawable.stat_sys_download)
                .setOngoing(true)
                .setShowWhen(false);
        if (contentIntent != null) {
            builder.setContentIntent(contentIntent);
        }
        return builder.build();
    }
}
