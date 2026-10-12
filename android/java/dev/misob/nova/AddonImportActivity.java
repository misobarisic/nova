package dev.misob.nova;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.lang.ref.WeakReference;
import java.util.concurrent.atomic.AtomicLong;

/** Framework-only document picker: cargo-apk2 supplies android.jar, no AndroidX. */
public final class AddonImportActivity extends Activity {
    private static final int PICK_ZIP = 0x5a;
    private static final int MAX_BYTES = 8 * 1024 * 1024;
    private static final AtomicLong pending = new AtomicLong();
    private static volatile WeakReference<AddonImportActivity> current = new WeakReference<>(null);
    private long request;
    private boolean launched;

    private static native void nativeResult(long request, byte[] bytes, int outcome);

    public static void start(Context context, long request) {
        pending.set(request);
        Intent intent = new Intent(context, AddonImportActivity.class);
        intent.putExtra("request", request);
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        context.startActivity(intent);
    }

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        request = getIntent().getLongExtra("request", 0);
        launched = state != null && state.getBoolean("launched");
        current = new WeakReference<>(this);
        if (pending.get() != request) {
            finish();
            return;
        }
        if (!launched) {
            launched = true;
            Intent picker = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            picker.addCategory(Intent.CATEGORY_OPENABLE);
            picker.setType("application/zip");
            picker.putExtra(Intent.EXTRA_MIME_TYPES, new String[] {
                "application/zip", "application/x-zip-compressed", "application/octet-stream"
            });
            try {
                startActivityForResult(picker, PICK_ZIP);
            } catch (android.content.ActivityNotFoundException missingZipPicker) {
                // Some document providers do not advertise ZIP support. Rust
                // validates the archive regardless of the provider's MIME type.
                picker.setType("*/*");
                picker.removeExtra(Intent.EXTRA_MIME_TYPES);
                try {
                    startActivityForResult(picker, PICK_ZIP);
                } catch (RuntimeException error) {
                    complete(request, null, 3);
                }
            } catch (RuntimeException error) {
                complete(request, null, 3);
            }
        }
    }

    @Override public void onSaveInstanceState(Bundle state) {
        state.putBoolean("launched", launched);
        super.onSaveInstanceState(state);
    }

    @Override public void onActivityResult(int code, int result, Intent data) {
        super.onActivityResult(code, result, data);
        if (code != PICK_ZIP) return;
        Uri uri = data == null ? null : data.getData();
        if (result != RESULT_OK || uri == null) {
            complete(request, null, 0);
            return;
        }
        final Context context = getApplicationContext();
        final long id = request;
        new Thread(() -> {
            try (InputStream input = context.getContentResolver().openInputStream(uri)) {
                if (input == null) throw new java.io.IOException("missing stream");
                ByteArrayOutputStream output = new ByteArrayOutputStream();
                byte[] buffer = new byte[16 * 1024];
                int total = 0;
                int count;
                while ((count = input.read(buffer)) != -1) {
                    if (pending.get() != id) return;
                    if (count > MAX_BYTES - total) {
                        complete(id, null, 2);
                        return;
                    }
                    output.write(buffer, 0, count);
                    total += count;
                }
                complete(id, output.toByteArray(), 1);
            } catch (Exception error) {
                complete(id, null, 3);
            }
        }, "NovaAddonImport").start();
    }

    private static void complete(long request, byte[] bytes, int outcome) {
        // Rotation and late worker results must never emit two native callbacks.
        if (!pending.compareAndSet(request, 0)) return;
        nativeResult(request, bytes, outcome);
        AddonImportActivity activity = current.get();
        if (activity != null && activity.request == request) {
            activity.runOnUiThread(activity::finish);
        }
    }

    @Override public void onDestroy() {
        if (!isChangingConfigurations() && isFinishing()) complete(request, null, 0);
        super.onDestroy();
    }
}
