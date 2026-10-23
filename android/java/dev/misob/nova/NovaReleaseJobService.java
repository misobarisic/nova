package dev.misob.nova;

import android.Manifest;
import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.job.JobInfo;
import android.app.job.JobParameters;
import android.app.job.JobScheduler;
import android.app.job.JobService;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.os.Build;
import org.json.JSONArray;
import java.util.concurrent.atomic.AtomicBoolean;

/** Persisted opportunistic scheduling: no exact alarm or foreground-service start.
 * Each pass recomputes civil 09:00 in the current zone, so boot, DST and timezone
 * changes need no stale wall-clock alarm restoration. */
public final class NovaReleaseJobService extends JobService {
    private static final int JOB = 0x55;
    private static final String CHANNEL = "nova-releases-v1";
    private static final String TAP = "nova.release.tap";
    private static final String ACK_PROFILE = "nova.release.profile";
    private static final String ACK_KEYS = "nova.release.keys";
    private static final AtomicBoolean RUNNING = new AtomicBoolean();
    private volatile boolean stopped;
    private static String pendingTap;
    static { System.loadLibrary("nova"); }

    public static void configure(Context context, boolean enabled) {
        JobScheduler scheduler = context.getSystemService(JobScheduler.class);
        if (scheduler == null) return;
        ComponentName component = new ComponentName(context,NovaReleaseJobService.class);
        JobInfo existing = scheduler.getPendingJob(JOB);
        if (!enabled) {
            if (existing != null && existing.getService().equals(component)) scheduler.cancel(JOB);
            return;
        }
        if (existing != null && !existing.getService().equals(component)) return;
        if (existing != null && existing.isPeriodic() && existing.getIntervalMillis()==15*60*1000L && existing.isPersisted()) return;
        scheduler.schedule(new JobInfo.Builder(JOB,component).setPeriodic(15*60*1000L).setPersisted(true).build());
    }
    public static boolean enable(Context context) {
        if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            if (context instanceof Activity) ((Activity)context).runOnUiThread(() -> ((Activity)context).requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS},0x54));
            return false;
        }
        return allowed(context);
    }
    public static boolean allowed(Context context) {
        if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return false;
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null || !manager.areNotificationsEnabled()) return false;
        NotificationChannel channel = manager.getNotificationChannel(CHANNEL);
        return channel == null || channel.getImportance() != NotificationManager.IMPORTANCE_NONE;
    }
    public static void notify(Context context,String profile,String title,String body,boolean alert,String event,String keys) {
        if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return;
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager == null) return;
        manager.createNotificationChannel(new NotificationChannel(CHANNEL,"Nova",NotificationManager.IMPORTANCE_DEFAULT));
        int id = 0x54000000 ^ profile.hashCode();
        if (body.isEmpty()) { manager.cancel(profile,id); return; }
        String target = new JSONArray().put(profile).put(event).toString();
        Intent intent = new Intent(context,NovaActivity.class).putExtra(TAP,target).putExtra(ACK_PROFILE,profile).putExtra(ACK_KEYS,keys).setAction("nova.release."+profile)
            .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP|Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent pending = PendingIntent.getActivity(context,id,intent,PendingIntent.FLAG_UPDATE_CURRENT|PendingIntent.FLAG_IMMUTABLE);
        Intent dismissed = new Intent(context,NovaReleaseDismissReceiver.class).setAction("nova.release.dismiss."+profile).putExtra(ACK_PROFILE,profile).putExtra(ACK_KEYS,keys);
        PendingIntent delete = PendingIntent.getBroadcast(context,id,dismissed,PendingIntent.FLAG_UPDATE_CURRENT|PendingIntent.FLAG_IMMUTABLE);
        Notification.Builder builder = new Notification.Builder(context,CHANNEL)
            .setSmallIcon(context.getApplicationInfo().icon).setContentTitle(title).setContentText(body)
            .setStyle(new Notification.BigTextStyle().bigText(body)).setContentIntent(pending)
            .setAutoCancel(true).setDeleteIntent(delete).setOnlyAlertOnce(!alert);
        // API 26+: group alert suppression mutes a newly reposted correction
        // even when the old notification was already dismissed. Channel sound
        // overrides setSound(null), so only-alert-once alone is insufficient.
        if (!alert) builder.setGroup("nova-release-silent").setGroupAlertBehavior(Notification.GROUP_ALERT_SUMMARY).setSound(null).setVibrate(null);
        manager.notify(profile,id,builder.build());
    }
    public static synchronized void captureTap(Context context,Intent intent) {
        if (intent != null && intent.hasExtra(TAP)) {
            pendingTap=intent.getStringExtra(TAP); intent.removeExtra(TAP);
            new Thread(() -> acknowledge(context,intent),"nova-release-open").start();
        }
    }
    public static void acknowledge(Context context,Intent intent) {
        String profile=intent.getStringExtra(ACK_PROFILE),keys=intent.getStringExtra(ACK_KEYS);
        if (profile != null && keys != null) nativeAcknowledge(context,profile,keys);
    }
    public static synchronized String consumeTap() { String tap=pendingTap; pendingTap=null; return tap; }
    @Override public boolean onStartJob(JobParameters params) {
        if (!RUNNING.compareAndSet(false,true)) return false;
        stopped=false; nativePrepare();
        new Thread(() -> { try { nativeRun(); } finally { if (!stopped) jobFinished(params,false); RUNNING.set(false); } },"nova-release-job").start();
        return true;
    }
    @Override public boolean onStopJob(JobParameters params) { stopped=true; nativeCancel(); return true; }
    private static native void nativeAcknowledge(Context context,String profile,String keys);
    private native void nativePrepare();
    private native void nativeCancel();
    private native void nativeRun();
}
