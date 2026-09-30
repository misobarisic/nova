package dev.misob.nova;

import android.app.job.JobInfo;
import android.app.job.JobParameters;
import android.app.job.JobScheduler;
import android.app.job.JobService;
import android.content.ComponentName;
import android.content.Context;
import android.util.Log;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Periodic background-sync job. JobScheduler wakes the process roughly every
 * 15 minutes (opportunistically less often under Doze), and this service runs
 * one bounded sync pass in Rust. No Activity is present, so the native side
 * initializes paths/storage from this Service (which is a Context) rather than
 * from android-activity.
 *
 * A background job must not start a foreground service (blocked since Android
 * 12), and the pass is bounded, so it runs entirely inside the job window.
 */
public class NovaSyncJobService extends JobService {
    private static final String TAG = "NovaSyncJob";
    private static final int JOB_ID = 0x52;
    private static final long PERIOD_MS = 15 * 60 * 1000L;
    private static final AtomicBoolean running = new AtomicBoolean(false);
    private static final AtomicBoolean stopped = new AtomicBoolean(false);

    static {
        // The native library is normally loaded by the NativeActivity; a
        // job-only process start must load it itself so the native method
        // below resolves.
        System.loadLibrary("nova");
    }

    /** Schedule (idempotent) the periodic sync job. */
    public static void schedule(Context context) {
        JobScheduler scheduler =
                (JobScheduler) context.getSystemService(Context.JOB_SCHEDULER_SERVICE);
        if (scheduler == null) {
            return;
        }
        ComponentName component = new ComponentName(context, NovaSyncJobService.class);
        JobInfo job = new JobInfo.Builder(JOB_ID, component)
                .setPeriodic(PERIOD_MS)
                .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
                .setPersisted(true)
                .build();
        scheduler.schedule(job);
    }

    /** Cancel the periodic sync job (sync disabled). */
    public static void cancel(Context context) {
        JobScheduler scheduler =
                (JobScheduler) context.getSystemService(Context.JOB_SCHEDULER_SERVICE);
        if (scheduler != null) {
            scheduler.cancel(JOB_ID);
        }
    }

    @Override
    public boolean onStartJob(JobParameters params) {
        if (!running.compareAndSet(false, true)) { return false; }
        stopped.set(false);
        nativePrepareSync();
        // Never block the main thread: run the pass on a worker and finish the
        // job when it returns.
        Thread worker = new Thread(() -> {
            try {
                String summary = nativeRunSync();
                Log.i(TAG, "background sync " + summary);
            } catch (Throwable error) {
                // A failed pass must not crash the process; the job is
                // rescheduled by the system.
                Log.e(TAG, "background sync failed", error);
            } finally {
                if (!stopped.getAndSet(true)) { jobFinished(params, false); }
                running.set(false);
            }
        }, "nova-sync-job");
        worker.start();
        return true;
    }

    @Override
    public boolean onStopJob(JobParameters params) {
        stopped.set(true);
        nativeCancelSync();
        // The system stopped us (e.g. constraints changed): reschedule.
        return true;
    }

    /**
     * Runs one bounded sync pass in Rust. `this` is the JobService Context.
     * Returns a classified completion/cancellation/failure summary for logging.
     */
    private native String nativeRunSync();
    private native void nativeCancelSync();
    private native void nativePrepareSync();
}
