package dev.misob.nova;

import android.app.KeyguardManager;
import android.app.NativeActivity;
import android.app.PendingIntent;
import android.app.PictureInPictureParams;
import android.app.RemoteAction;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.content.res.Configuration;
import android.graphics.Rect;
import android.graphics.drawable.Icon;
import android.os.Build;
import android.os.Bundle;
import android.os.PowerManager;
import android.util.Rational;
import android.view.View;
import java.util.Collections;

/** NativeActivity's surface and event loop remain owned by android-activity. */
public final class NovaActivity extends NativeActivity {
    private static final String TRANSPORT = "dev.misob.nova.PIP_TRANSPORT";
    private volatile boolean eligible;
    private volatile boolean entering;
    private boolean paused = true;
    private boolean stopped;
    private volatile boolean screenOff;
    private boolean pipVisible;
    private boolean playbackActive;
    private boolean autoRotatePlayback = true;
    private boolean orientationManaged;
    private int videoWidth = 16;
    private int videoHeight = 9;
    private static native void nativePipEvent(int event);

    private final Runnable applyPlaybackOrientation = () -> {
        if (!orientationManaged || stopped || screenOff || locked() || isFinishing() || isInMultiWindowMode()
                || (getResources().getConfiguration().uiMode & Configuration.UI_MODE_TYPE_MASK)
                    == Configuration.UI_MODE_TYPE_TELEVISION) return;
        // Release to the physical sensor, not a forced portrait request: the
        // catalog must stay landscape when the phone is still held sideways.
        int orientation = !autoRotatePlayback ? ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
                : playbackActive ? ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
                : ActivityInfo.SCREEN_ORIENTATION_SENSOR;
        if (getRequestedOrientation() != orientation) setRequestedOrientation(orientation);
        if (!autoRotatePlayback) orientationManaged = false;
    };

    private void schedulePlaybackOrientation() {
        View decor = getWindow().getDecorView();
        // Resolving another source closes/reopens the native player in one
        // turn. Coalesce rapid handoffs to avoid briefly rotating the catalog.
        decor.removeCallbacks(applyPlaybackOrientation);
        decor.postOnAnimation(applyPlaybackOrientation);
    }

    /** Independent of PiP eligibility, pause state and the system rotation lock. */
    public void setPlaybackActive(boolean active, boolean autoRotate) {
        runOnUiThread(() -> {
            if (playbackActive == active && autoRotatePlayback == autoRotate && orientationManaged) return;
            playbackActive = active;
            autoRotatePlayback = autoRotate;
            if (active && autoRotate) orientationManaged = true;
            schedulePlaybackOrientation();
        });
    }

    private final BroadcastReceiver receiver = new BroadcastReceiver() {
        @Override public void onReceive(Context context, Intent intent) {
            if (Intent.ACTION_SCREEN_OFF.equals(intent.getAction())) {
                screenOff = true;
                pipVisible = false;
                entering = false;
                eligible = false;
                applyPipParams();
                nativePipEvent(4);
            } else if (Intent.ACTION_SCREEN_ON.equals(intent.getAction())
                    || Intent.ACTION_USER_PRESENT.equals(intent.getAction())) {
                restoreVisiblePictureInPicture();
            } else if (TRANSPORT.equals(intent.getAction()) && isInPictureInPictureMode()
                    && eligible && !screenOff && !locked()) {
                nativePipEvent(intent.getBooleanExtra("paused", true) ? 2 : 3);
            }
        }
    };

    @Override protected void onCreate(Bundle state) {
        super.onCreate(state);
        NovaReleaseJobService.captureTap(this,getIntent());
        IntentFilter filter = new IntentFilter(TRANSPORT);
        filter.addAction(Intent.ACTION_SCREEN_OFF);
        filter.addAction(Intent.ACTION_SCREEN_ON);
        filter.addAction(Intent.ACTION_USER_PRESENT);
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED);
        } else {
            registerReceiver(receiver, filter);
        }
        getWindow().getDecorView().addOnLayoutChangeListener((v, l, t, r, b, ol, ot, or, ob) -> {
            if (eligible && !isInPictureInPictureMode()) applyPipParams();
        });
    }

    @Override protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        NovaReleaseJobService.captureTap(this,intent);
    }

    private boolean locked() {
        KeyguardManager manager = (KeyguardManager) getSystemService(KEYGUARD_SERVICE);
        return manager != null && manager.isKeyguardLocked();
    }

    private boolean supportsPip() {
        return getPackageManager().hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE);
    }

    private void restoreVisiblePictureInPicture() {
        PowerManager power = (PowerManager) getSystemService(POWER_SERVICE);
        if (pipVisible || stopped || isFinishing() || !isInPictureInPictureMode()
                || locked() || power == null || !power.isInteractive()) return;
        // Unlock can restore a pinned Activity to Started/Paused without ever
        // calling onResume. Restore native recovery too, then let its current
        // source/setting checks republish eligibility and the Play action.
        screenOff = false;
        pipVisible = true;
        nativePipEvent(5);
    }

    /** Called from the native UI thread; publish eligibility before its next Home event. */
    public void updatePictureInPicture(boolean active, boolean isPaused, int width, int height) {
        runOnUiThread(() -> {
            boolean next = active && !screenOff && !isFinishing();
            boolean changed = eligible != next || paused != isPaused
                    || videoWidth != width || videoHeight != height;
            eligible = next;
            paused = isPaused;
            videoWidth = width;
            videoHeight = height;
            if (changed) applyPipParams();
        });
    }

    private PictureInPictureParams params() {
        // The framework rejects extreme movie/portrait ratios outside this range.
        float ratio = videoHeight > 0 ? (float) videoWidth / videoHeight : 16.0f / 9.0f;
        Rational aspect = ratio <= 1.0f / 2.39f ? new Rational(100, 239)
                : ratio >= 2.39f ? new Rational(239, 100)
                : new Rational(Math.max(1, videoWidth), Math.max(1, videoHeight));
        PictureInPictureParams.Builder builder = new PictureInPictureParams.Builder()
                .setAspectRatio(aspect);
        if (Build.VERSION.SDK_INT >= 31) {
            builder.setAutoEnterEnabled(eligible && !paused && !screenOff && !locked());
            builder.setSeamlessResizeEnabled(true);
        }
        Rect source = new Rect();
        if (!isInPictureInPictureMode() && getWindow().getDecorView().getGlobalVisibleRect(source)) {
            // mpv letterboxes the video inside this same native window.
            int w = Math.min(source.width(), Math.round(source.height() * ratio));
            int h = Math.min(source.height(), Math.round(source.width() / ratio));
            int x = source.left + (source.width() - w) / 2;
            int y = source.top + (source.height() - h) / 2;
            builder.setSourceRectHint(new Rect(x, y, x + w, y + h));
        }
        Intent command = new Intent(TRANSPORT).setPackage(getPackageName())
                .putExtra("paused", !paused);
        PendingIntent pending = PendingIntent.getBroadcast(this, paused ? 1 : 2, command,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        int icon = paused ? android.R.drawable.ic_media_play : android.R.drawable.ic_media_pause;
        // Resolve by name: cargo-apk2 compiles Java before aapt's generated R.java.
        int label = getResources().getIdentifier(paused ? "pip_play" : "pip_pause", "string", getPackageName());
        RemoteAction action = new RemoteAction(Icon.createWithResource(this, icon),
                getString(label), getString(label), pending);
        action.setEnabled(eligible && !screenOff);
        builder.setActions(Collections.singletonList(action));
        return builder.build();
    }

    private void applyPipParams() {
        if (!supportsPip() || isFinishing()) return;
        try {
            setPictureInPictureParams(params());
        } catch (IllegalArgumentException | IllegalStateException ignored) {
            // Unsupported/vendor-disabled PiP must leave ordinary playback usable.
            entering = false;
        }
    }

    /** Pause can precede the PiP mode callback during the Home animation. */
    public boolean pictureInPictureKeepsPlaying() {
        return !screenOff && !locked() && (isInPictureInPictureMode() || entering);
    }

    @Override protected void onUserLeaveHint() {
        super.onUserLeaveHint();
        if (!eligible || paused || screenOff || locked() || !supportsPip()) return;
        entering = true;
        if (Build.VERSION.SDK_INT < 31) {
            try {
                entering = enterPictureInPictureMode(params());
            } catch (IllegalArgumentException | IllegalStateException ignored) {
                entering = false;
            }
        }
        if (entering) {
            nativePipEvent(1);
            // A denied PiP permission can cancel automatic entry after Pause.
            getWindow().getDecorView().postDelayed(() -> {
                entering = false;
                if (!isInPictureInPictureMode()) {
                    nativePipEvent(0);
                    if (stopped || !hasWindowFocus()) nativePipEvent(4);
                }
            }, 1500);
        }
    }

    @Override public void onPictureInPictureModeChanged(boolean pip, Configuration config) {
        super.onPictureInPictureModeChanged(pip, config);
        entering = false;
        nativePipEvent(pip ? 1 : 0);
        if (pip) restoreVisiblePictureInPicture();
        else pipVisible = false;
        if (!pip && stopped) nativePipEvent(4);
        schedulePlaybackOrientation();
    }

    @Override public void onMultiWindowModeChanged(boolean multiWindow, Configuration config) {
        super.onMultiWindowModeChanged(multiWindow, config);
        schedulePlaybackOrientation();
    }

    @Override public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) schedulePlaybackOrientation();
    }

    @Override protected void onResume() {
        stopped = false;
        screenOff = false;
        entering = false;
        pipVisible = false;
        super.onResume();
        nativePipEvent(0);
        applyPipParams();
        schedulePlaybackOrientation();
    }

    @Override protected void onStop() {
        stopped = true;
        pipVisible = false;
        getWindow().getDecorView().removeCallbacks(applyPlaybackOrientation);
        entering = false;
        // PiP is visible while paused but must stop when dismissed or locked.
        nativePipEvent(4);
        super.onStop();
    }

    @Override protected void onStart() {
        super.onStart();
        stopped = false;
        restoreVisiblePictureInPicture();
    }

    @Override protected void onDestroy() {
        getWindow().getDecorView().removeCallbacks(applyPlaybackOrientation);
        unregisterReceiver(receiver);
        super.onDestroy();
    }
}
