package dev.misob.nova;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.lang.ref.WeakReference;
import java.util.UUID;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicReference;

/** SAF import/export, including cloud-backed document providers. */
public final class DocumentActivity extends Activity {
    private static final int PICK = 0x6b;
    private static final int MAX_BYTES = 128 * 1024 * 1024;
    private static final class Request {
        final long id;
        final String token = UUID.randomUUID().toString();
        // Avoid putting archive bytes in a Binder transaction or saved-state Bundle.
        final byte[] exportBytes;
        final AtomicBoolean reading = new AtomicBoolean();
        WeakReference<DocumentActivity> current = new WeakReference<>(null);
        Request(long id, byte[] exportBytes) { this.id = id; this.exportBytes = exportBytes; }
    }
    private static final AtomicReference<Request> pending = new AtomicReference<>();
    private Request operation;
    private boolean launched;
    private static native void nativeResult(long request, byte[] bytes, int outcome, String message);
    public static void start(Context context, long request, String name, String mime, byte[] bytes) {
        Request operation = new Request(request, bytes);
        // Rust admits one picker at a time. Keep that ownership if another
        // launch races it, rather than replacing another request's export data.
        if (!pending.compareAndSet(null, operation)) throw new IllegalStateException("A document picker is already open");
        Intent intent = new Intent(context, DocumentActivity.class);
        intent.putExtra("request", request);
        intent.putExtra("token", operation.token);
        intent.putExtra("name", name);
        intent.putExtra("mime", mime);
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        try { context.startActivity(intent); }
        catch (RuntimeException error) { pending.compareAndSet(operation, null); throw error; }
    }
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        long request = getIntent().getLongExtra("request", 0);
        operation = pending.get();
        // A process restart has neither export bytes nor a native callback.
        // Restored picker results must not be mistaken for a fresh import.
        if (operation == null || operation.id != request || !operation.token.equals(getIntent().getStringExtra("token"))) { operation = null; finish(); return; }
        operation.current = new WeakReference<>(this);
        launched = state != null && state.getBoolean("launched");
        if (!launched) {
            launched = true;
            boolean save = operation.exportBytes != null;
            Intent picker = new Intent(save ? Intent.ACTION_CREATE_DOCUMENT : Intent.ACTION_OPEN_DOCUMENT);
            picker.addCategory(Intent.CATEGORY_OPENABLE);
            picker.setType(save ? getIntent().getStringExtra("mime") : "*/*");
            if (save) picker.putExtra(Intent.EXTRA_TITLE, getIntent().getStringExtra("name"));
            try { startActivityForResult(picker, PICK); }
            catch (RuntimeException error) { complete(operation, null, 2, error.toString()); }
        }
    }
    @Override public void onSaveInstanceState(Bundle state) { state.putBoolean("launched", launched); super.onSaveInstanceState(state); }
    @Override public void onActivityResult(int code, int result, Intent data) {
        super.onActivityResult(code, result, data);
        if (code != PICK) return;
        final Request request = operation;
        if (request == null || pending.get() != request) { finish(); return; }
        // Rotation or a duplicate result must not start a second stream writer.
        if (!request.reading.compareAndSet(false, true)) return;
        Uri uri = data == null ? null : data.getData();
        if (result != RESULT_OK || uri == null) { complete(request, null, 0, ""); return; }
        final Context context = getApplicationContext();
        new Thread(() -> {
            try {
                if (request.exportBytes != null) {
                    if (pending.get() != request) return;
                    try (OutputStream output = context.getContentResolver().openOutputStream(uri, "wt")) {
                        if (output == null) throw new java.io.IOException("Missing document output");
                        byte[] bytes = request.exportBytes;
                        for (int offset = 0; offset < bytes.length; offset += 16384) {
                            if (pending.get() != request) return;
                            output.write(bytes, offset, Math.min(16384, bytes.length - offset));
                        }
                        output.flush();
                    }
                    complete(request, new byte[0], 1, "");
                } else {
                    try (InputStream input = context.getContentResolver().openInputStream(uri)) {
                        if (input == null) throw new java.io.IOException("Missing document input");
                        ByteArrayOutputStream output = new ByteArrayOutputStream();
                        byte[] buffer = new byte[16384];
                        int count;
                        while ((count = input.read(buffer)) != -1) {
                            if (pending.get() != request) return;
                            if (count > MAX_BYTES - output.size()) throw new java.io.IOException("Document exceeds the size limit");
                            output.write(buffer, 0, count);
                        }
                        complete(request, output.toByteArray(), 1, "");
                    }
                }
            } catch (Exception error) { complete(request, null, 2, error.toString()); }
        }, "NovaDocument").start();
    }
    private static void complete(Request request, byte[] bytes, int outcome, String message) {
        // Identity protects a subsequent request, even if a stale worker returns
        // after cancellation. Only the winning completion calls native code.
        if (request == null || !pending.compareAndSet(request, null)) return;
        try { nativeResult(request.id, bytes, outcome, message); }
        finally {
            new Handler(Looper.getMainLooper()).post(() -> {
                // Resolve on the main thread, after any rotation handoff. A
                // replacement created after this runs sees pending=null above.
                DocumentActivity activity = request.current.get();
                if (activity != null && activity.operation == request) activity.finish();
            });
        }
    }
    @Override public void onDestroy() {
        if (!isChangingConfigurations() && isFinishing()) complete(operation, null, 0, "");
        if (operation != null && operation.current.get() == this) operation.current = new WeakReference<>(null);
        super.onDestroy();
    }
}
