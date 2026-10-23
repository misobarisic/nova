package dev.misob.nova;

import android.app.job.JobInfo;
import android.app.job.JobParameters;
import android.app.job.JobScheduler;
import android.app.job.JobService;
import android.content.BroadcastReceiver;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.os.BatteryManager;
import android.util.Log;
import java.util.concurrent.atomic.AtomicBoolean;

/** Bounded offline planning/transfers with no Activity and no FGS start. */
public final class NovaOfflineJobService extends JobService {
    private static final int JOB_ID=0x54;
    private static final AtomicBoolean running=new AtomicBoolean();
    private static final AtomicBoolean stopped=new AtomicBoolean();
    private static boolean observing;
    static { System.loadLibrary("nova"); }
    private static native void nativeEnvironment(int flags);
    private native void nativePrepare();
    private native void nativeCancel();
    private native String nativeRun();
    public static synchronized void configure(Context context,boolean enabled,boolean charging) {
        Context app=context.getApplicationContext();
        observe(app); reportEnvironment(app);
        JobScheduler scheduler=(JobScheduler)app.getSystemService(Context.JOB_SCHEDULER_SERVICE);
        if(scheduler==null)return;
        ComponentName component=new ComponentName(app,NovaOfflineJobService.class);
        JobInfo old=scheduler.getPendingJob(JOB_ID);
        if(!enabled){if(old!=null && component.equals(old.getService()))scheduler.cancel(JOB_ID);return;}
        if(old!=null && !component.equals(old.getService())){Log.e("NovaOffline","job ID belongs to another component; refusing replacement");return;}
        if(old!=null && old.isRequireCharging()==charging
                && old.getNetworkType()==JobInfo.NETWORK_TYPE_UNMETERED
                && old.isRequireStorageNotLow() && old.isPersisted()
                && old.isPeriodic() && old.getIntervalMillis()==15*60*1000L)return;
        JobInfo job=new JobInfo.Builder(JOB_ID,component)
            .setRequiredNetworkType(JobInfo.NETWORK_TYPE_UNMETERED).setRequiresCharging(charging)
            .setRequiresStorageNotLow(true).setPersisted(true).setPeriodic(15*60*1000L).build();
        if(scheduler.schedule(job)!=JobScheduler.RESULT_SUCCESS)Log.w("NovaOffline","job scheduling deferred");
    }
    private static synchronized void observe(Context app) {
        if(observing)return;observing=true;
        ConnectivityManager cm=(ConnectivityManager)app.getSystemService(Context.CONNECTIVITY_SERVICE);
        if(cm!=null)cm.registerDefaultNetworkCallback(new ConnectivityManager.NetworkCallback(){
            @Override public void onAvailable(Network network){reportEnvironment(app);}
            @Override public void onLost(Network network){reportEnvironment(app);}
            @Override public void onCapabilitiesChanged(Network network,NetworkCapabilities caps){reportEnvironment(app);}
        });
        app.registerReceiver(new BroadcastReceiver(){@Override public void onReceive(Context c,Intent i){reportEnvironment(app);}},new IntentFilter(Intent.ACTION_BATTERY_CHANGED));
    }
    private static void reportEnvironment(Context context) {
        int flags=0;
        ConnectivityManager cm=(ConnectivityManager)context.getSystemService(Context.CONNECTIVITY_SERVICE);
        if(cm!=null){NetworkCapabilities caps=cm.getNetworkCapabilities(cm.getActiveNetwork());
            if(caps!=null && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) && caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)){
                flags|=1;if(caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)||caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET))flags|=2;
                if(!cm.isActiveNetworkMetered())flags|=4;
            }
        }
        Intent battery=context.registerReceiver(null,new IntentFilter(Intent.ACTION_BATTERY_CHANGED));
        if(battery!=null){int status=battery.getIntExtra(BatteryManager.EXTRA_STATUS,-1);if(status==BatteryManager.BATTERY_STATUS_CHARGING||status==BatteryManager.BATTERY_STATUS_FULL)flags|=8;}
        nativeEnvironment(flags);
    }
    @Override public boolean onStartJob(JobParameters params) {
        if(!running.compareAndSet(false,true))return false;
        stopped.set(false);nativePrepare();observe(getApplicationContext());reportEnvironment(this);
        new Thread(()->{try{Log.i("NovaOffline",nativeRun());}finally{running.set(false);if(!stopped.get())jobFinished(params,false);}},"nova-offline-job").start();
        return true;
    }
    @Override public boolean onStopJob(JobParameters params) {stopped.set(true);nativeCancel();return true;}
}
