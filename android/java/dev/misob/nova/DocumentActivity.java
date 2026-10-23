package dev.misob.nova;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.concurrent.atomic.AtomicLong;

/** SAF import/export, including cloud-backed document providers. */
public final class DocumentActivity extends Activity {
    private static final int PICK = 0x6b;
    private static final int MAX_BYTES = 128 * 1024 * 1024;
    private static final AtomicLong pending = new AtomicLong();
    // Avoid putting archive bytes in a Binder transaction or saved-state Bundle.
    private static volatile byte[] exportBytes;
    private long request;
    private boolean launched;
    private static native void nativeResult(long request, byte[] bytes, int outcome, String message);
    public static void start(Context context, long request, String name, String mime, byte[] bytes) {
        pending.set(request);
        exportBytes = bytes;
        Intent intent = new Intent(context, DocumentActivity.class);
        intent.putExtra("request", request);
        intent.putExtra("name", name);
        intent.putExtra("mime", mime);
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        context.startActivity(intent);
    }
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        request = getIntent().getLongExtra("request", 0);
        launched = state != null && state.getBoolean("launched");
        if (pending.get() != request) { finish(); return; }
        if (!launched) {
            launched = true;
            boolean save = exportBytes != null;
            Intent picker = new Intent(save ? Intent.ACTION_CREATE_DOCUMENT : Intent.ACTION_OPEN_DOCUMENT);
            picker.addCategory(Intent.CATEGORY_OPENABLE);
            picker.setType(save ? getIntent().getStringExtra("mime") : "*/*");
            if (save) picker.putExtra(Intent.EXTRA_TITLE, getIntent().getStringExtra("name"));
            try { startActivityForResult(picker, PICK); }
            catch (RuntimeException error) { complete(null, 2, error.toString()); }
        }
    }
    @Override public void onSaveInstanceState(Bundle state) { state.putBoolean("launched", launched); super.onSaveInstanceState(state); }
    @Override public void onActivityResult(int code, int result, Intent data) {
        super.onActivityResult(code, result, data);
        if (code != PICK) return;
        Uri uri = data == null ? null : data.getData();
        if (result != RESULT_OK || uri == null) { complete(null, 0, ""); return; }
        final byte[] bytes = exportBytes;
        new Thread(() -> {
            try {
                if (bytes != null) {
                    try (OutputStream output = getContentResolver().openOutputStream(uri, "wt")) {
                        if (output == null) throw new java.io.IOException("Missing document output");
                        output.write(bytes);
                        output.flush();
                    }
                    complete(new byte[0], 1, "");
                } else {
                    try (InputStream input = getContentResolver().openInputStream(uri)) {
                        if (input == null) throw new java.io.IOException("Missing document input");
                        ByteArrayOutputStream output = new ByteArrayOutputStream();
                        byte[] buffer = new byte[16384];
                        int count;
                        while ((count = input.read(buffer)) != -1) {
                            if (count > MAX_BYTES - output.size()) throw new java.io.IOException("Document exceeds the size limit");
                            output.write(buffer, 0, count);
                        }
                        complete(output.toByteArray(), 1, "");
                    }
                }
            } catch (Exception error) { complete(null, 2, error.toString()); }
        }, "NovaDocument").start();
    }
    private void complete(byte[] bytes, int outcome, String message) {
        if (!pending.compareAndSet(request, 0)) return;
        exportBytes = null;
        nativeResult(request, bytes, outcome, message);
        runOnUiThread(this::finish);
    }
    @Override public void onDestroy() {
        if (!isChangingConfigurations() && isFinishing()) complete(null, 0, "");
        super.onDestroy();
    }
}
