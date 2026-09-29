package dev.misob.nova;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Matrix;
import android.graphics.RectF;
import android.graphics.SurfaceTexture;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Log;
import android.util.Size;
import android.view.Gravity;
import android.view.Surface;
import android.view.TextureView;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.TextView;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Full-screen camera QR scanner for pairing invites.
 *
 * The camera pipeline lives here (framework Camera2 only — cargo-apk2 cannot
 * bundle an AAR, so no CameraX/ZXing/ML Kit); each frame's luma plane is handed
 * to Rust through {@link #nativeOnFrame}, which decodes with `rqrr` and, on a
 * valid invite, joins on the UI thread. A hit makes the native call return true
 * and this activity finishes.
 *
 * Launched from Rust (`android_qr::start_scan`), never exported.
 */
public class QrScanActivity extends Activity {
    private static final String TAG = "NovaQrScan";
    private static final int REQUEST_CAMERA = 0x51;
    /** Analyse at most one frame per interval; a decode is a few ms of CPU. */
    private static final long FRAME_INTERVAL_MS = 150;
    /** Target analysis resolution; QR needs no more and small keeps decode fast. */
    private static final Size TARGET_ANALYSIS = new Size(1280, 720);

    private TextureView previewView;
    private ImageReader imageReader;
    private CameraDevice camera;
    private CameraCaptureSession session;
    private HandlerThread backgroundThread;
    private Handler backgroundHandler;
    private String cameraId;
    private Size previewSize;
    private Size analysisSize;
    private int sensorOrientation = 0;
    private final AtomicBoolean finished = new AtomicBoolean(false);
    private long lastFrameMs = 0L;

    /** Launch the scanner. Called from Rust; always from the UI/Activity. */
    public static void start(Context context) {
        Intent intent = new Intent(context, QrScanActivity.class);
        // Only an Activity context can start without a task flag; the app
        // passes the NativeActivity, but stay safe if that ever changes.
        if (!(context instanceof Activity)) {
            intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        }
        context.startActivity(intent);
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        setTitle("Scan invite code");

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(0xFF000000);

        previewView = new TextureView(this);
        root.addView(previewView, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        previewView.setSurfaceTextureListener(new TextureView.SurfaceTextureListener() {
            @Override public void onSurfaceTextureAvailable(SurfaceTexture texture, int width, int height) {
                // The camera may already be open (surface arrived late); build
                // the session, else start the camera.
                if (camera != null) {
                    createSession();
                } else {
                    openCamera();
                }
            }
            @Override public void onSurfaceTextureSizeChanged(SurfaceTexture texture, int width, int height) {
                configureTransform(width, height);
            }
            @Override public boolean onSurfaceTextureDestroyed(SurfaceTexture texture) { return true; }
            @Override public void onSurfaceTextureUpdated(SurfaceTexture texture) {}
        });

        // Hint + cancel overlay.
        TextView hint = new TextView(this);
        hint.setText("Point the camera at the other device's invite QR code");
        hint.setTextColor(0xFFFFFFFF);
        hint.setGravity(Gravity.CENTER);
        hint.setPadding(48, 48, 48, 48);
        FrameLayout.LayoutParams hintParams = new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        hintParams.gravity = Gravity.TOP;
        root.addView(hint, hintParams);

        Button cancel = new Button(this);
        cancel.setText("Cancel");
        cancel.setOnClickListener(v -> finish());
        FrameLayout.LayoutParams cancelParams = new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        cancelParams.gravity = Gravity.BOTTOM | Gravity.CENTER_HORIZONTAL;
        cancelParams.bottomMargin = 96;
        root.addView(cancel, cancelParams);

        setContentView(root);

        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[] { Manifest.permission.CAMERA }, REQUEST_CAMERA);
        }
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] results) {
        if (requestCode != REQUEST_CAMERA) {
            super.onRequestPermissionsResult(requestCode, permissions, results);
            return;
        }
        if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            // The surface may already be available; otherwise the listener
            // fires and opens the camera.
            if (previewView.isAvailable()) {
                openCamera();
            }
        } else {
            Log.w(TAG, "camera permission denied");
            finish();
        }
    }

    // -----------------------------------------------------------------------
    // Camera2 pipeline
    // -----------------------------------------------------------------------

    private void startBackgroundThread() {
        if (backgroundThread != null) {
            return;
        }
        backgroundThread = new HandlerThread("nova-qr-scan");
        backgroundThread.start();
        backgroundHandler = new Handler(backgroundThread.getLooper());
    }

    private void stopBackgroundThread() {
        if (backgroundThread == null) {
            return;
        }
        backgroundThread.quitSafely();
        try {
            backgroundThread.join();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        backgroundThread = null;
        backgroundHandler = null;
    }

    private boolean hasCameraPermission() {
        return checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED;
    }

    private void openCamera() {
        if (camera != null || finished.get() || !hasCameraPermission()) {
            return;
        }
        CameraManager manager = (CameraManager) getSystemService(Context.CAMERA_SERVICE);
        try {
            for (String id : manager.getCameraIdList()) {
                CameraCharacteristics chars = manager.getCameraCharacteristics(id);
                Integer facing = chars.get(CameraCharacteristics.LENS_FACING);
                if (facing != null && facing == CameraCharacteristics.LENS_FACING_BACK) {
                    cameraId = id;
                    Integer orientation = chars.get(CameraCharacteristics.SENSOR_ORIENTATION);
                    sensorOrientation = orientation == null ? 0 : orientation;
                    StreamConfigurationMap map = chars.get(
                            CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
                    if (map == null) {
                        continue;
                    }
                    analysisSize = pickSize(map.getOutputSizes(android.graphics.ImageFormat.YUV_420_888),
                            TARGET_ANALYSIS);
                    previewSize = pickSize(map.getOutputSizes(SurfaceTexture.class), TARGET_ANALYSIS);
                    break;
                }
            }
        } catch (CameraAccessException e) {
            Log.e(TAG, "camera enumeration failed", e);
        }
        if (cameraId == null) {
            Log.w(TAG, "no back camera");
            finish();
            return;
        }

        // The analysis reader is created once; its listener runs on the
        // background handler and throttles to FRAME_INTERVAL_MS.
        startBackgroundThread();
        imageReader = ImageReader.newInstance(
                analysisSize.getWidth(), analysisSize.getHeight(),
                android.graphics.ImageFormat.YUV_420_888, 2);
        imageReader.setOnImageAvailableListener(reader -> onFrame(reader), backgroundHandler);

        try {
            manager.openCamera(cameraId, new CameraDevice.StateCallback() {
                @Override public void onOpened(CameraDevice device) {
                    camera = device;
                    createSession();
                }
                @Override public void onDisconnected(CameraDevice device) {
                    device.close();
                    camera = null;
                }
                @Override public void onError(CameraDevice device, int error) {
                    Log.e(TAG, "camera error " + error);
                    device.close();
                    camera = null;
                    finish();
                }
            }, backgroundHandler);
        } catch (CameraAccessException | SecurityException e) {
            Log.e(TAG, "openCamera failed", e);
            finish();
        }
    }

    private void createSession() {
        if (camera == null || imageReader == null || !previewView.isAvailable()) {
            return;
        }
        SurfaceTexture texture = previewView.getSurfaceTexture();
        if (texture == null) {
            return;
        }
        texture.setDefaultBufferSize(previewSize.getWidth(), previewSize.getHeight());
        Surface previewSurface = new Surface(texture);
        try {
            CaptureRequest.Builder builder =
                    camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
            builder.addTarget(previewSurface);
            builder.addTarget(imageReader.getSurface());
            // Continuous AF where the device supports it; some devices reject
            // the mode, so don't let it abort session creation.
            try {
                builder.set(CaptureRequest.CONTROL_AF_MODE,
                        CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE);
            } catch (IllegalArgumentException e) {
                Log.w(TAG, "continuous AF unsupported", e);
            }

            List<Surface> targets = new ArrayList<>(Arrays.asList(previewSurface, imageReader.getSurface()));
            camera.createCaptureSession(targets, new CameraCaptureSession.StateCallback() {
                @Override public void onConfigured(CameraCaptureSession configured) {
                    if (camera == null) {
                        return;
                    }
                    session = configured;
                    try {
                        session.setRepeatingRequest(builder.build(), null, backgroundHandler);
                    } catch (CameraAccessException e) {
                        Log.e(TAG, "repeating request failed", e);
                    }
                }
                @Override public void onConfigureFailed(CameraCaptureSession configured) {
                    Log.e(TAG, "capture session config failed");
                }
            }, backgroundHandler);
        } catch (CameraAccessException e) {
            Log.e(TAG, "createCaptureSession failed", e);
        }
        configureTransform(previewView.getWidth(), previewView.getHeight());
    }

    /** Throttled frame handler: extract the luma plane and ask Rust to decode. */
    private void onFrame(ImageReader reader) {
        Image image = reader.acquireLatestImage();
        if (image == null) {
            return;
        }
        try {
            long now = System.currentTimeMillis();
            if (finished.get() || now - lastFrameMs < FRAME_INTERVAL_MS) {
                return;
            }
            lastFrameMs = now;
            Image.Plane plane = image.getPlanes()[0];
            ByteBuffer buffer = plane.getBuffer();
            int width = image.getWidth();
            int height = image.getHeight();
            int rowStride = plane.getRowStride();
            // Copy out the luma rows (the buffer may have padding beyond the
            // last row, and getBuffer() can expose a slice with an offset).
            byte[] luma = new byte[rowStride * height];
            buffer.rewind();
            int length = Math.min(buffer.remaining(), luma.length);
            buffer.get(luma, 0, length);
            boolean hit = nativeOnFrame(luma, width, height, rowStride);
            if (hit && finished.compareAndSet(false, true)) {
                runOnUiThread(this::finish);
            }
        } finally {
            image.close();
        }
    }

    /** Keep the preview upright: map the sensor orientation onto the view. */
    private void configureTransform(int viewWidth, int viewHeight) {
        if (previewView == null || previewSize == null || viewWidth == 0 || viewHeight == 0) {
            return;
        }
        int rotation = getWindowManager().getDefaultDisplay().getRotation();
        Matrix matrix = new Matrix();
        RectF viewRect = new RectF(0, 0, viewWidth, viewHeight);
        RectF bufferRect = new RectF(0, 0, previewSize.getHeight(), previewSize.getWidth());
        float centerX = viewRect.centerX();
        float centerY = viewRect.centerY();
        if (rotation == Surface.ROTATION_90 || rotation == Surface.ROTATION_270) {
            bufferRect.offset(centerX - bufferRect.centerX(), centerY - bufferRect.centerY());
            matrix.setRectToRect(viewRect, bufferRect, Matrix.ScaleToFit.FILL);
            float scale = Math.max(
                    (float) viewHeight / previewSize.getHeight(),
                    (float) viewWidth / previewSize.getWidth());
            matrix.postScale(scale, scale, centerX, centerY);
            matrix.postRotate(90 * (rotation - 2), centerX, centerY);
        } else if (rotation == Surface.ROTATION_180) {
            matrix.postRotate(180, centerX, centerY);
        }
        previewView.setTransform(matrix);
    }

    private static Size pickSize(Size[] sizes, Size target) {
        if (sizes == null || sizes.length == 0) {
            return target;
        }
        Size best = sizes[0];
        long bestScore = Long.MAX_VALUE;
        long targetArea = (long) target.getWidth() * target.getHeight();
        for (Size size : sizes) {
            long area = (long) size.getWidth() * size.getHeight();
            // Prefer the smallest size at least as large as the target, else
            // the largest below it.
            long score = area >= targetArea ? area : (targetArea - area) + targetArea;
            if (score < bestScore) {
                bestScore = score;
                best = size;
            }
        }
        return best;
    }

    @Override
    protected void onResume() {
        super.onResume();
        if (previewView != null && previewView.isAvailable()) {
            openCamera();
        }
    }

    @Override
    protected void onPause() {
        closeCamera();
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        closeCamera();
        super.onDestroy();
    }

    private void closeCamera() {
        if (session != null) {
            session.close();
            session = null;
        }
        if (camera != null) {
            camera.close();
            camera = null;
        }
        if (imageReader != null) {
            imageReader.close();
            imageReader = null;
        }
        stopBackgroundThread();
    }

    /** Decode one frame in Rust; returns true when an invite ticket was found. */
    private native boolean nativeOnFrame(byte[] luma, int width, int height, int rowStride);
}
