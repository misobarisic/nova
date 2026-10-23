package dev.misob.nova;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** Notification dismissal is a bounded durable acknowledgment, including after
 * process death. It never opens an Activity, refreshes metadata or starts a service. */
public final class NovaReleaseDismissReceiver extends BroadcastReceiver {
    @Override public void onReceive(Context context, Intent intent) {
        PendingResult result=goAsync();
        new Thread(() -> {
            try { NovaReleaseJobService.acknowledge(context,intent); }
            finally { result.finish(); }
        },"nova-release-dismiss").start();
    }
}
