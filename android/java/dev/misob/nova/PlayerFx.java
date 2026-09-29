package dev.misob.nova;

import android.app.Activity;
import android.content.Context;
import android.media.AudioManager;
import android.os.Handler;
import android.os.Looper;
import android.view.Window;
import android.view.WindowManager;

/**
 * Player-gesture system bridges for swipe volume/brightness.
 *
 * Called from Rust (`src/app/android_player.rs`), which loads this class
 * through the app class loader and invokes the static methods below. All
 * work here uses framework APIs only (no AAR, so cargo-apk2 can compile it).
 */
public final class PlayerFx {
    private PlayerFx() {
    }

    /**
     * Step the music-stream volume up ({@code direction > 0}) or down. The
     * system volume panel is the readout. Binder call, safe from any thread.
     */
    public static void adjustVolume(Context context, int direction) {
        AudioManager audio = (AudioManager) context.getSystemService(Context.AUDIO_SERVICE);
        if (audio == null) {
            return;
        }
        audio.adjustStreamVolume(
                AudioManager.STREAM_MUSIC,
                direction > 0 ? AudioManager.ADJUST_RAISE : AudioManager.ADJUST_LOWER,
                AudioManager.FLAG_SHOW_UI);
    }

    /**
     * Live window brightness (0..1), or -1 while the window follows the
     * system default. A plain field read on a params copy. Takes the
     * activity itself: the app context is not necessarily an activity.
     */
    public static float getBrightness(Activity activity) {
        if (activity == null) {
            return -1.0f;
        }
        return activity.getWindow().getAttributes().screenBrightness;
    }

    /**
     * Set the session brightness (-1 hands the window back to the system
     * default). Takes the activity itself (see above). `Window` calls must
     * run on the UI thread while the caller may be any thread, so this
     * marshals through the main looper.
     */
    public static void setBrightness(final Activity activity, final float brightness) {
        new Handler(Looper.getMainLooper()).post(new Runnable() {
            @Override
            public void run() {
                if (activity == null || activity.isFinishing()) {
                    return;
                }
                Window window = activity.getWindow();
                WindowManager.LayoutParams params = window.getAttributes();
                params.screenBrightness = brightness;
                window.setAttributes(params);
            }
        });
    }
}
