package com.tongchuan;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.Color;
import android.hardware.camera2.CameraAccessException;
import android.hardware.camera2.CameraCaptureSession;
import android.hardware.camera2.CameraCharacteristics;
import android.hardware.camera2.CameraDevice;
import android.hardware.camera2.CameraManager;
import android.hardware.camera2.CaptureRequest;
import android.hardware.camera2.params.StreamConfigurationMap;
import android.media.Image;
import android.media.ImageReader;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.util.Size;
import android.view.Gravity;
import android.view.Surface;
import android.view.TextureView;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.TextView;

import com.google.zxing.BarcodeFormat;
import com.google.zxing.BinaryBitmap;
import com.google.zxing.ChecksumException;
import com.google.zxing.DecodeHintType;
import com.google.zxing.FormatException;
import com.google.zxing.MultiFormatReader;
import com.google.zxing.NotFoundException;
import com.google.zxing.PlanarYUVLuminanceSource;
import com.google.zxing.RGBLuminanceSource;
import com.google.zxing.Result;
import com.google.zxing.common.HybridBinarizer;

import java.io.InputStream;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.EnumMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * v0.20 扫码页：Camera2 实时预览 + zxing 解码。
 * 为什么自己写预览而不是引 zxing-android-embedded：后者自带一套 Activity 与资源，
 * 本项目是三端共用一个内核的轻量应用，多一层 UI 依赖不值得。
 * 页面只做一件事：扫到首个能解析的二维码就把文本回给调用方，解析与校验交给 Rust。
 */
public class ScanActivity extends Activity {

    public static final String EXTRA_RESULT = "payload";
    private static final int REQ_CAMERA = 200;
    private static final int REQ_GALLERY = 201;
    /** 相册图片解码前的长边上限：手机原图动辄 4000px，全尺寸解码既慢又容易 OOM */
    private static final int IMAGE_MAX_EDGE = 1600;
    private static final int TARGET_W = 1280;
    private static final int TARGET_H = 720;

    private TextureView preview;
    private TextView hint;
    private HandlerThread bgThread;
    private Handler bg;
    private CameraDevice camera;
    private CameraCaptureSession session;
    private ImageReader reader;
    private final MultiFormatReader qr = new MultiFormatReader();
    /**
     * 相册图片专用解码器。
     * 为什么不用上面那个 MultiFormatReader：它会把校验失败一律包成 NotFoundException，
     * 分不清「图里没有二维码」和「有码但糊了」；直接用 QRCodeReader 才能分别给出提示。
     */
    private final com.google.zxing.qrcode.QRCodeReader qrImageReader =
            new com.google.zxing.qrcode.QRCodeReader();
    private final Map<DecodeHintType, Object> imageHints = new EnumMap<>(DecodeHintType.class);
    /** 一帧还没解完就不再塞下一帧：解码是 CPU 活，堆积只会发热掉帧 */
    private final AtomicBoolean busy = new AtomicBoolean(false);
    /** 已经拿到结果就不再解码，避免同一张码触发多次回调 */
    private volatile boolean done;
    private String cameraId;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);
        preview = new TextureView(this);
        root.addView(preview, new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT));

        hint = new TextView(this);
        hint.setText("把对方的配对二维码放进画面\n扫到即自动返回");
        hint.setTextColor(Color.WHITE);
        hint.setTextSize(16);
        hint.setGravity(Gravity.CENTER);
        hint.setBackgroundColor(0x66000000);
        hint.setPadding(24, 32, 24, 32);

        // 相册入口放在底部：相机被拒、或二维码在另一张截图里时，这是唯一的扫码通道，
        // 走系统文件选择器，不需要任何存储/相机权限。
        Button pick = new Button(this);
        pick.setText("从相册选择二维码");
        pick.setOnClickListener(v -> openGallery());

        LinearLayout bottom = new LinearLayout(this);
        bottom.setOrientation(LinearLayout.VERTICAL);
        bottom.addView(hint, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));
        bottom.addView(pick, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));
        FrameLayout.LayoutParams hl = new FrameLayout.LayoutParams(
                FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.WRAP_CONTENT);
        hl.gravity = Gravity.BOTTOM;
        root.addView(bottom, hl);
        setContentView(root);

        bgThread = new HandlerThread("tc-scan");
        bgThread.start();
        bg = new Handler(bgThread.getLooper());

        // 只认二维码：别的条码格式在局域网配对里没有意义，限制后解码更快
        Map<DecodeHintType, Object> hints = new EnumMap<>(DecodeHintType.class);
        List<BarcodeFormat> formats = new ArrayList<>();
        formats.add(BarcodeFormat.QR_CODE);
        hints.put(DecodeHintType.POSSIBLE_FORMATS, formats);
        hints.put(DecodeHintType.TRY_HARDER, Boolean.TRUE);
        qr.setHints(hints);
        // 相册图没有尺寸上限，必须 TRY_HARDER；格式已经固定是二维码
        imageHints.put(DecodeHintType.TRY_HARDER, Boolean.TRUE);

        preview.setSurfaceTextureListener(new TextureView.SurfaceTextureListener() {
            @Override
            public void onSurfaceTextureAvailable(android.graphics.SurfaceTexture st, int w, int h) {
                openCamera();
            }

            @Override
            public void onSurfaceTextureSizeChanged(android.graphics.SurfaceTexture st, int w, int h) {
            }

            @Override
            public boolean onSurfaceTextureDestroyed(android.graphics.SurfaceTexture st) {
                return true;
            }

            @Override
            public void onSurfaceTextureUpdated(android.graphics.SurfaceTexture st) {
            }
        });

        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.CAMERA}, REQ_CAMERA);
        }
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] perms, int[] results) {
        super.onRequestPermissionsResult(code, perms, results);
        if (code != REQ_CAMERA) {
            return;
        }
        if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            openCamera();
        } else {
            // 相机被拒不是死路：返回后仍可在「设备与保活」里手动发现并选目标
            hint.setText("未获得相机权限\n可返回后用「发现附近的设备」手动选目标");
            android.widget.Toast.makeText(this,
                    "相机权限被拒，可改用「发现附近的设备」手动发送",
                    android.widget.Toast.LENGTH_LONG).show();
        }
    }

    private void openCamera() {
        if (camera != null || done) {
            return;
        }
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            return;
        }
        CameraManager cm = (CameraManager) getSystemService(CAMERA_SERVICE);
        try {
            for (String id : cm.getCameraIdList()) {
                Integer facing = cm.getCameraCharacteristics(id)
                        .get(CameraCharacteristics.LENS_FACING);
                if (facing != null && facing == CameraCharacteristics.LENS_FACING_BACK) {
                    cameraId = id;
                    break;
                }
            }
            if (cameraId == null) {
                hint.setText("这台设备没有可用的后置相机");
                return;
            }
            cm.openCamera(cameraId, deviceCallback, bg);
        } catch (CameraAccessException | SecurityException e) {
            hint.setText("打不开相机：" + e.getMessage());
        }
    }

    private final CameraDevice.StateCallback deviceCallback = new CameraDevice.StateCallback() {
        @Override
        public void onOpened(CameraDevice dev) {
            camera = dev;
            startSession();
        }

        @Override
        public void onDisconnected(CameraDevice dev) {
            dev.close();
            camera = null;
        }

        @Override
        public void onError(CameraDevice dev, int error) {
            dev.close();
            camera = null;
            runOnUiThread(() -> hint.setText("相机出错（" + error + "），请返回重试"));
        }
    };

    private void startSession() {
        if (camera == null || preview.getSurfaceTexture() == null) {
            return;
        }
        try {
            CameraCharacteristics ch = ((CameraManager) getSystemService(CAMERA_SERVICE))
                    .getCameraCharacteristics(cameraId);
            StreamConfigurationMap map =
                    ch.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP);
            if (map == null) {
                hint.setText("相机能力读取失败");
                return;
            }
            Size pv = nearest(map.getOutputSizes(android.graphics.SurfaceTexture.class),
                    TARGET_W, TARGET_H);
            Size dec = nearest(map.getOutputSizes(android.graphics.ImageFormat.YUV_420_888),
                    TARGET_W, TARGET_H);
            if (pv == null || dec == null) {
                hint.setText("相机不支持所需的预览格式");
                return;
            }
            preview.getSurfaceTexture().setDefaultBufferSize(pv.getWidth(), pv.getHeight());
            Surface previewSurface = new Surface(preview.getSurfaceTexture());
            reader = ImageReader.newInstance(dec.getWidth(), dec.getHeight(),
                    android.graphics.ImageFormat.YUV_420_888, 2);
            reader.setOnImageAvailableListener(this::onFrame, bg);

            List<Surface> outputs = new ArrayList<>();
            outputs.add(previewSurface);
            outputs.add(reader.getSurface());
            camera.createCaptureSession(outputs, new CameraCaptureSession.StateCallback() {
                @Override
                public void onConfigured(CameraCaptureSession s) {
                    session = s;
                    try {
                        CaptureRequest.Builder b =
                                camera.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW);
                        b.addTarget(previewSurface);
                        b.addTarget(reader.getSurface());
                        // 连续对焦：近距离扫屏时固定焦距经常糊到解不出来
                        b.set(CaptureRequest.CONTROL_AF_MODE,
                                CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE);
                        session.setRepeatingRequest(b.build(), null, bg);
                    } catch (CameraAccessException e) {
                        runOnUiThread(() -> hint.setText("启动预览失败：" + e.getMessage()));
                    }
                }

                @Override
                public void onConfigureFailed(CameraCaptureSession s) {
                    runOnUiThread(() -> hint.setText("相机预览配置失败，请返回重试"));
                }
            }, bg);
        } catch (CameraAccessException e) {
            hint.setText("启动相机失败：" + e.getMessage());
        }
    }

    /** 取长宽比最接近目标、且不超过目标像素量的那一档，避免 4K 预览白烧 CPU。 */
    private static Size nearest(Size[] sizes, int w, int h) {
        if (sizes == null || sizes.length == 0) {
            return null;
        }
        Size best = sizes[0];
        long bestScore = Long.MAX_VALUE;
        for (Size s : sizes) {
            long score = Math.abs((long) s.getWidth() - w) + Math.abs((long) s.getHeight() - h);
            if (score < bestScore) {
                bestScore = score;
                best = s;
            }
        }
        return best;
    }

    private void onFrame(ImageReader r) {
        Image img = null;
        try {
            img = r.acquireLatestImage();
        } catch (IllegalStateException e) {
            return;
        }
        if (img == null) {
            return;
        }
        // acquire 之后必须 close，否则 ImageReader 很快就再也拿不到帧
        try {
            if (done || !busy.compareAndSet(false, true)) {
                return;
            }
            String text = decode(img);
            if (text != null) {
                finishWith(text);
            }
        } finally {
            img.close();
            busy.set(false);
        }
    }

    /** 只取 Y 平面做灰度解码：QR 识别不需要彩色，省掉一次 YUV→RGB 转换。 */
    private String decode(Image img) {
        try {
            Image.Plane plane = img.getPlanes()[0];
            ByteBuffer buf = plane.getBuffer();
            int w = img.getWidth();
            int h = img.getHeight();
            int rowStride = plane.getRowStride();
            int pixelStride = plane.getPixelStride();
            byte[] data = new byte[w * h];
            if (pixelStride == 1 && rowStride == w) {
                buf.get(data, 0, w * h);
            } else {
                // 有的机型每行末尾有对齐填充，必须按 rowStride 逐行取，否则整幅图会斜切
                byte[] row = new byte[rowStride];
                for (int y = 0; y < h; y++) {
                    buf.position(y * rowStride);
                    int n = Math.min(rowStride, buf.remaining());
                    buf.get(row, 0, n);
                    for (int x = 0; x < w; x++) {
                        data[y * w + x] = row[x * pixelStride];
                    }
                }
            }
            PlanarYUVLuminanceSource src =
                    new PlanarYUVLuminanceSource(data, w, h, 0, 0, w, h, false);
            BinaryBitmap bitmap = new BinaryBitmap(new HybridBinarizer(src));
            // 相册解码在另一条线程上用同一个 reader，必须互斥，否则 hints/状态会串
            synchronized (qr) {
                try {
                    Result result = qr.decodeWithState(bitmap);
                    return result == null ? null : result.getText();
                } finally {
                    qr.reset();
                }
            }
        } catch (NotFoundException | ArrayIndexOutOfBoundsException e) {
            return null;
        }
    }

    private void finishWith(String text) {
        if (done) {
            return;
        }
        done = true;
        runOnUiThread(() -> {
            setResult(RESULT_OK, new Intent().putExtra(EXTRA_RESULT, text));
            finish();
        });
    }

    /** 从系统文件选择器选一张图；不需要相机或存储权限，相机被拒时也能用。 */
    private void openGallery() {
        Intent i = new Intent(Intent.ACTION_GET_CONTENT);
        i.setType("image/*");
        i.addCategory(Intent.CATEGORY_OPENABLE);
        try {
            startActivityForResult(Intent.createChooser(i, "选择二维码图片"), REQ_GALLERY);
        } catch (Exception e) {
            hint.setText("打不开相册：" + e.getMessage());
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQ_GALLERY) {
            return;
        }
        if (resultCode != RESULT_OK || data == null || data.getData() == null) {
            return;
        }
        final Uri uri = data.getData();
        hint.setText("正在识别图片…");
        // 解码是纯 CPU 活，放后台线程，避免大图把界面卡成假死
        new Thread(() -> decodeImageFromUri(uri), "tc-scan-image").start();
    }

    /**
     * 解码相册图片里的二维码，成功走与相机扫码完全相同的回填路径。
     * 先按长边降采样：手机原图动辄 4000px，全尺寸解码既慢又容易 OOM。
     */
    private void decodeImageFromUri(Uri uri) {
        Bitmap bmp = null;
        try {
            BitmapFactory.Options bounds = new BitmapFactory.Options();
            bounds.inJustDecodeBounds = true;
            try (InputStream in = getContentResolver().openInputStream(uri)) {
                BitmapFactory.decodeStream(in, null, bounds);
            }
            int sample = 1;
            while (bounds.outWidth / sample > IMAGE_MAX_EDGE
                    || bounds.outHeight / sample > IMAGE_MAX_EDGE) {
                sample *= 2;
            }
            BitmapFactory.Options opt = new BitmapFactory.Options();
            opt.inSampleSize = sample;
            try (InputStream in = getContentResolver().openInputStream(uri)) {
                bmp = BitmapFactory.decodeStream(in, null, opt);
            }
            if (bmp == null) {
                failImage("这张图片读不出来，换一张试试");
                return;
            }
            int w = bmp.getWidth();
            int h = bmp.getHeight();
            int[] pixels = new int[w * h];
            bmp.getPixels(pixels, 0, w, 0, 0, w, h);
            BinaryBitmap bitmap = new BinaryBitmap(
                    new HybridBinarizer(new RGBLuminanceSource(w, h, pixels)));
            Result result;
            synchronized (qrImageReader) {
                result = qrImageReader.decode(bitmap, imageHints);
            }
            finishWith(result.getText());
        } catch (NotFoundException e) {
            // 连定位图案都没找到：多半不是二维码，也可能整张糊到没有边缘
            failImage("图片里没找到二维码\n换一张更清晰的图，或直接用相机扫");
        } catch (ChecksumException | FormatException e) {
            // 定位到了但内容校验不过：典型是拍糊、过度缩放或被裁掉一角
            failImage("二维码识别失败（图片模糊或被裁切）\n换一张清晰的图再试");
        } catch (Exception e) {
            failImage("识别失败：" + e.getMessage());
        } finally {
            if (bmp != null) {
                bmp.recycle();
            }
        }
    }

    private void failImage(String msg) {
        runOnUiThread(() -> hint.setText(msg));
    }

    @Override
    protected void onResume() {
        super.onResume();
        // 首次进来要先弹相机授权，授权弹窗会把 Activity 暂停、相机会被关掉；
        // 授权通过或从后台回来时这里把相机重新拉起来，否则画面会一直是黑的
        if (preview != null && preview.isAvailable() && camera == null) {
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
        if (bgThread != null) {
            bgThread.quitSafely();
            bgThread = null;
        }
        super.onDestroy();
    }

    private void closeCamera() {
        try {
            if (session != null) {
                session.close();
                session = null;
            }
        } catch (Exception ignored) {
        }
        try {
            if (camera != null) {
                camera.close();
                camera = null;
            }
        } catch (Exception ignored) {
        }
        try {
            if (reader != null) {
                reader.close();
                reader = null;
            }
        } catch (Exception ignored) {
        }
    }
}
