package dev.misob.nova;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.Uri;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.PowerManager;
import android.os.SystemClock;

import java.lang.ref.WeakReference;
import java.util.HashMap;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;

/** Keeps an explicitly started external receiver session and its Rust media
 * relay alive independently of NativeActivity. No source URL or account data
 * crosses this interface. This service does not play local audio or enter PiP.
 */
public final class NovaCastService extends Service {
    private static final String CHANNEL = "nova.casting";
    private static final int NOTIFICATION = 0x57;
    private static final String ACTION = "dev.misob.nova.CAST_ACTION";
    private static final AtomicInteger nextRequest = new AtomicInteger();
    private static final ConcurrentHashMap<Integer, Start> requests = new ConcurrentHashMap<>();
    private static final HashMap<Long, WifiManager.MulticastLock> discoveries = new HashMap<>();
    private static WeakReference<NovaCastService> instance = new WeakReference<>(null);

    private static native void nativeCommand(long generation, int command);

    private static final class Start {
        final long generation;
        final CountDownLatch ready = new CountDownLatch(1);
        volatile boolean success;
        volatile boolean cancelled;
        Start(long generation) { this.generation = generation; }
    }

    private final Handler handler = new Handler(Looper.getMainLooper());
    private long generation;
    private long heartbeat;
    private boolean foreground;
    private boolean paused = true;
    private boolean canPause;
    private String device = "";
    // All display strings are supplied in Nova's selected language by Rust.
    private String[] labels = { "", "", "", "", "", "" };
    private PowerManager.WakeLock wakeLock;
    private MediaSession mediaSession;
    private ConnectivityManager connectivity;
    private boolean networkCallbackRegistered;

    private final ConnectivityManager.NetworkCallback networkCallback =
            new ConnectivityManager.NetworkCallback() {
                private Network current;
                @Override public void onAvailable(Network network) {
                    if (current != null && !current.equals(network)) reportNetworkChange();
                    current = network;
                }
                @Override public void onLost(Network network) {
                    if (network.equals(current)) {
                        current = null;
                        reportNetworkChange();
                    }
                }
            };

    private void reportNetworkChange() {
        handler.post(() -> {
            if (foreground && generation != 0) nativeCommand(generation, 5);
        });
    }

    private final Runnable watchdog = new Runnable() {
        @Override public void run() {
            if (!foreground) return;
            // Rust polls and renews while paused as well. An abandoned worker
            // must not leave an ongoing service or wake lock indefinitely.
            if (SystemClock.elapsedRealtime() - heartbeat > 90_000) {
                stopSelf();
                return;
            }
            handler.postDelayed(this, 15_000);
        }
    };

    /** Wait off the main thread until actual foreground promotion. No remote
     * LOAD is sent until Rust receives true. Timed-out requests cannot later
     * create an orphan service.
     */
    public static boolean start(Context context, String device, boolean paused,
            long generation, String[] labels) {
        if (generation == 0 || Looper.myLooper() == Looper.getMainLooper()) return false;
        int id = nextRequest.incrementAndGet();
        Start request = new Start(generation);
        requests.put(id, request);
        boolean ready = false;
        try {
            Intent intent = new Intent(context, NovaCastService.class)
                    .putExtra("request", id).putExtra("generation", generation)
                    .putExtra("device", device).putExtra("paused", paused)
                    .putExtra("labels", labels);
            context.startForegroundService(intent);
            ready = request.ready.await(5, TimeUnit.SECONDS) && request.success;
            return ready;
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            return false;
        } catch (RuntimeException error) {
            return false;
        } finally {
            request.cancelled = !ready;
            requests.remove(id);
            if (!ready) stop(context, generation);
        }
    }

    public static void update(Context context, String device, boolean paused,
            long generation, String[] labels) {
        NovaCastService service = instance.get();
        if (service == null) return;
        service.handler.post(() -> {
            if (!service.foreground || service.generation != generation) return;
            service.heartbeat = SystemClock.elapsedRealtime();
            service.applyState(device, paused, labels);
            service.refresh();
        });
    }

    public static void setCanPause(long generation, boolean allowed) {
        NovaCastService service = instance.get();
        if (service != null) service.handler.post(() -> {
            if (!service.foreground || service.generation != generation) return;
            service.canPause = allowed;
            service.applyState(service.device, service.paused, service.labels);
            service.refresh();
        });
    }

    public static void stop(Context context, long generation) {
        if (generation == 0) return;
        for (Start request : requests.values()) {
            if (request.generation == generation) request.cancelled = true;
        }
        NovaCastService service = instance.get();
        if (service != null) service.handler.post(() -> {
            if (service.generation == generation) service.stopSelf();
        });
    }

    public static boolean acquireDiscovery(Context context, long id) {
        synchronized (discoveries) {
            if (discoveries.containsKey(id)) return true;
            // A finite search owns each lease. Bound a buggy caller rather
            // than retaining arbitrarily many platform locks.
            if (discoveries.size() >= 4) return false;
            try {
                WifiManager wifi = (WifiManager) context.getApplicationContext()
                        .getSystemService(Context.WIFI_SERVICE);
                if (wifi == null) return true; // Ethernet-only Android device.
                WifiManager.MulticastLock lock = wifi.createMulticastLock("nova:cast-search");
                lock.setReferenceCounted(false);
                lock.acquire();
                discoveries.put(id, lock);
                return true;
            } catch (RuntimeException error) {
                return false;
            }
        }
    }

    public static void releaseDiscovery(long id) {
        synchronized (discoveries) {
            WifiManager.MulticastLock lock = discoveries.remove(id);
            if (lock != null && lock.isHeld()) lock.release();
        }
    }

    @Override public void onCreate() {
        super.onCreate();
        instance = new WeakReference<>(this);
        PowerManager power = getSystemService(PowerManager.class);
        if (power != null) {
            wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "nova:casting");
            wakeLock.setReferenceCounted(false);
        }
        connectivity = getSystemService(ConnectivityManager.class);
        if (connectivity != null) {
            try {
                connectivity.registerDefaultNetworkCallback(networkCallback);
                networkCallbackRegistered = true;
            } catch (RuntimeException error) {
                networkCallbackRegistered = false;
            }
        }
        mediaSession = new MediaSession(this, "nova:casting");
        mediaSession.setCallback(new MediaSession.Callback() {
            @Override public void onPlay() { send(1); }
            @Override public void onPause() { if (canPause) send(2); }
            @Override public void onStop() { send(3); }
        }, handler);
        // No audio focus: sound is rendered by the selected external device.
        mediaSession.setFlags(MediaSession.FLAG_HANDLES_MEDIA_BUTTONS
                | MediaSession.FLAG_HANDLES_TRANSPORT_CONTROLS);
    }

    private void send(int command) {
        if (foreground && generation != 0) nativeCommand(generation, command);
    }

    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && ACTION.equals(intent.getAction())) {
            if (foreground && generation == intent.getLongExtra("generation", 0)) {
                send(intent.getIntExtra("command", 0));
            }
            // A stale PendingIntent can recreate the process after its route
            // died. It cannot reconstruct a session or retain an idle service.
            if (!foreground) stopSelf(startId);
            return START_NOT_STICKY;
        }
        Start request = intent == null ? null : requests.get(intent.getIntExtra("request", 0));
        if (request == null || request.cancelled) {
            if (!foreground) stopSelf(startId);
            return START_NOT_STICKY;
        }
        try {
            generation = request.generation;
            canPause = false;
            heartbeat = SystemClock.elapsedRealtime();
            applyState(intent.getStringExtra("device"), intent.getBooleanExtra("paused", true),
                    intent.getStringArrayExtra("labels"));
            NotificationManager manager = getSystemService(NotificationManager.class);
            if (manager == null) throw new IllegalStateException("Notification manager unavailable");
            manager.createNotificationChannel(new NotificationChannel(
                    CHANNEL, labels[0], NotificationManager.IMPORTANCE_LOW));
            Notification notification = notification();
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(NOTIFICATION, notification,
                        ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE);
            } else {
                startForeground(NOTIFICATION, notification);
            }
            foreground = true;
            mediaSession.setActive(true);
            handler.removeCallbacks(watchdog);
            handler.postDelayed(watchdog, 15_000);
            // A timed-out native caller may already have cancelled the setup.
            if (request.cancelled) stopSelf(startId);
            else request.success = true;
        } catch (RuntimeException error) {
            stopSelf(startId);
        } finally {
            request.ready.countDown();
        }
        // Active routes and source leases are memory-only; process death cannot
        // reconstruct them. Never restart casting automatically.
        return START_NOT_STICKY;
    }

    private void applyState(String name, boolean nextPaused, String[] translated) {
        if (translated != null && translated.length == 6) labels = translated.clone();
        device = name == null ? "" : name.substring(0, Math.min(name.length(), 200));
        paused = nextPaused;
        if (wakeLock != null) {
            if (paused) {
                if (wakeLock.isHeld()) wakeLock.release();
            } else {
                // A real timeout covers a stalled Java/native update path.
                // Renewed by the session heartbeat while active.
                wakeLock.acquire(120_000);
            }
        }
        if (mediaSession != null) {
            mediaSession.setMetadata(new MediaMetadata.Builder()
                    .putString(MediaMetadata.METADATA_KEY_TITLE, device)
                    .putString(MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE, labels[0]).build());
            mediaSession.setPlaybackState(new PlaybackState.Builder()
                    .setActions(PlaybackState.ACTION_PLAY | PlaybackState.ACTION_STOP
                            | (canPause ? PlaybackState.ACTION_PAUSE : 0))
                    .setState(paused ? PlaybackState.STATE_PAUSED : PlaybackState.STATE_PLAYING,
                            PlaybackState.PLAYBACK_POSITION_UNKNOWN, paused ? 0f : 1f).build());
        }
    }

    private void refresh() {
        NotificationManager manager = getSystemService(NotificationManager.class);
        if (manager != null) manager.notify(NOTIFICATION, notification());
    }

    private PendingIntent action(int command) {
        Intent intent = new Intent(this, NovaCastService.class).setAction(ACTION)
                .setData(Uri.parse("nova-cast://session/" + generation + "/" + command))
                .putExtra("generation", generation).putExtra("command", command);
        return PendingIntent.getService(this, command, intent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
    }

    private Notification notification() {
        Notification.Builder builder = new Notification.Builder(this, CHANNEL)
                .setContentTitle(device).setContentText(paused ? labels[2] : labels[1])
                .setSmallIcon(android.R.drawable.ic_media_play)
                .setOngoing(true).setShowWhen(false).setOnlyAlertOnce(true)
                .setCategory(Notification.CATEGORY_TRANSPORT);
        boolean transport = paused || canPause;
        if (transport) builder.addAction(new Notification.Action.Builder(
                        paused ? android.R.drawable.ic_media_play : android.R.drawable.ic_media_pause,
                        paused ? labels[3] : labels[4], action(paused ? 1 : 2)).build());
        builder.addAction(new Notification.Action.Builder(android.R.drawable.ic_menu_close_clear_cancel,
                        labels[5], action(3)).build());
        if (mediaSession != null) builder.setStyle(new Notification.MediaStyle()
                .setMediaSession(mediaSession.getSessionToken())
                .setShowActionsInCompactView(transport ? new int[] { 0, 1 } : new int[] { 0 }));
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        if (launch != null) {
            launch.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
                    .setAction("dev.misob.nova.CAST_CONTROLS." + generation)
                    .putExtra("nova_cast_generation", generation);
            builder.setContentIntent(PendingIntent.getActivity(this, NOTIFICATION, launch,
                    PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE));
        }
        return builder.build();
    }

    /** Called by the Activity on cold start or notification re-entry. The
     * native generation guard ignores old notification intents.
     */
    public static void captureTap(Intent intent) {
        if (intent == null) return;
        long id = intent.getLongExtra("nova_cast_generation", 0);
        if (id != 0) {
            intent.removeExtra("nova_cast_generation");
            nativeCommand(id, 4);
        }
    }

    @Override public void onDestroy() {
        long previous = generation;
        generation = 0;
        foreground = false;
        handler.removeCallbacksAndMessages(null);
        if (networkCallbackRegistered && connectivity != null) {
            try { connectivity.unregisterNetworkCallback(networkCallback); }
            catch (RuntimeException ignored) { }
        }
        if (mediaSession != null) {
            mediaSession.setActive(false);
            mediaSession.release();
            mediaSession = null;
        }
        if (wakeLock != null && wakeLock.isHeld()) wakeLock.release();
        wakeLock = null;
        stopForeground(STOP_FOREGROUND_REMOVE);
        if (instance.get() == this) instance = new WeakReference<>(null);
        if (previous != 0) nativeCommand(previous, 6);
        super.onDestroy();
    }

    @Override public IBinder onBind(Intent intent) { return null; }
}
