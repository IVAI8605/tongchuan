package com.tongchuan

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageFormat
import android.graphics.SurfaceTexture
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.StreamConfigurationMap
import android.media.Image
import android.media.ImageReader
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.util.Size
import android.view.Surface
import android.view.TextureView
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.ChecksumException
import com.google.zxing.DecodeHintType
import com.google.zxing.FormatException
import com.google.zxing.MultiFormatReader
import com.google.zxing.NotFoundException
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.Result
import com.google.zxing.common.HybridBinarizer
import com.tongchuan.ui.theme.TongchuanTheme
import java.io.InputStream
import java.nio.ByteBuffer
import java.util.EnumMap
import java.util.concurrent.atomic.AtomicBoolean

/**
 * v0.20 扫码页：Camera2 实时预览 + zxing 解码。
 *
 * 为什么自己写预览而不是引 zxing-android-embedded：后者自带一套 Activity 与资源，
 * 本项目是三端共用一个内核的轻量应用，多一层 UI 依赖不值得。
 * 页面只做一件事：扫到首个能解析的二维码就把文本回给调用方，解析与校验交给 Rust。
 *
 * 2026-11-19 UI 重构：预览仍用 TextureView（相机预览没有 Compose 原生控件），
 * 界面外壳换成 Compose + Material 3，相机与解码逻辑一行未改。
 */
class ScanActivity : ComponentActivity() {

    companion object {
        const val EXTRA_RESULT = "payload"
        private const val REQ_CAMERA = 200
        private const val REQ_GALLERY = 201

        /** 相册图片解码前的长边上限：手机原图动辄 4000px，全尺寸解码既慢又容易 OOM */
        private const val IMAGE_MAX_EDGE = 1600
        private const val TARGET_W = 1280
        private const val TARGET_H = 720
    }

    private lateinit var preview: TextureView

    /** 提示语走 Compose 状态：相机回调在后台线程，更新前切回主线程 */
    private val hint = mutableStateOf("把对方的配对二维码放进画面\n扫到即自动返回")

    private var bgThread: HandlerThread? = null
    private var bg: Handler? = null
    private var camera: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var reader: ImageReader? = null

    private val qr = MultiFormatReader()

    /**
     * 相册图片专用解码器。
     * 为什么不用上面那个 MultiFormatReader：它会把校验失败一律包成 NotFoundException，
     * 分不清「图里没有二维码」和「有码但糊了」；直接用 QRCodeReader 才能分别给出提示。
     */
    private val qrImageReader = com.google.zxing.qrcode.QRCodeReader()
    private val imageHints = EnumMap<DecodeHintType, Any>(DecodeHintType::class.java)

    /** 一帧还没解完就不再塞下一帧：解码是 CPU 活，堆积只会发热掉帧 */
    private val busy = AtomicBoolean(false)
    /** 已经拿到结果就不再解码，避免同一张码触发多次回调 */
    @Volatile private var done = false
    private var cameraId: String? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        preview = TextureView(this)
        bgThread = HandlerThread("tc-scan").also { it.start() }
        bg = Handler(bgThread!!.looper)

        // 只认二维码：别的条码格式在局域网配对里没有意义，限制后解码更快
        val hints = EnumMap<DecodeHintType, Any>(DecodeHintType::class.java)
        hints[DecodeHintType.POSSIBLE_FORMATS] = listOf(BarcodeFormat.QR_CODE)
        hints[DecodeHintType.TRY_HARDER] = true
        qr.setHints(hints)
        // 相册图没有尺寸上限，必须 TRY_HARDER；格式已经固定是二维码
        imageHints[DecodeHintType.TRY_HARDER] = true

        preview.surfaceTextureListener = object : TextureView.SurfaceTextureListener {
            override fun onSurfaceTextureAvailable(st: SurfaceTexture, w: Int, h: Int) {
                openCamera()
            }

            override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, w: Int, h: Int) {}

            override fun onSurfaceTextureDestroyed(st: SurfaceTexture): Boolean = true

            override fun onSurfaceTextureUpdated(st: SurfaceTexture) {}
        }

        setContent {
            // 扫码页固定深色：预览是实景画面，浅色外壳会刺眼
            TongchuanTheme(darkTheme = true) {
                Box(
                    modifier = Modifier.fillMaxSize()
                        .background(Color.Black),
                ) {
                    AndroidView(
                        factory = { preview },
                        modifier = Modifier.fillMaxSize(),
                    )
                    Column(
                        modifier = Modifier
                            .align(Alignment.BottomCenter)
                            .fillMaxWidth()
                            .background(MaterialTheme.colorScheme.surface.copy(alpha = 0.88f))
                            .padding(16.dp),
                    ) {
                        Text(
                            text = hint.value,
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurface,
                        )
                        Button(
                            onClick = { openGallery() },
                            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                        ) { Text("从相册选择二维码") }
                    }
                }
            }
        }

        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.CAMERA), REQ_CAMERA)
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode != REQ_CAMERA) {
            return
        }
        if (grantResults.isNotEmpty() && grantResults[0] == PackageManager.PERMISSION_GRANTED) {
            openCamera()
        } else {
            // 相机被拒不是死路：返回后仍可在「设备」页手动发现并选目标
            hint.value = "未获得相机权限\n可返回后用「发现附近的设备」手动选目标"
            android.widget.Toast.makeText(
                this,
                "相机权限被拒，可改用「发现附近的设备」手动发送",
                android.widget.Toast.LENGTH_LONG,
            ).show()
        }
    }

    private fun openCamera() {
        if (camera != null || done) {
            return
        }
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            return
        }
        val cm = getSystemService(CAMERA_SERVICE) as CameraManager
        try {
            for (id in cm.cameraIdList) {
                val facing = cm.getCameraCharacteristics(id)
                    .get(CameraCharacteristics.LENS_FACING)
                if (facing != null && facing == CameraCharacteristics.LENS_FACING_BACK) {
                    cameraId = id
                    break
                }
            }
            val id = cameraId
            if (id == null) {
                hint.value = "这台设备没有可用的后置相机"
                return
            }
            cm.openCamera(id, deviceCallback, bg)
        } catch (e: Exception) {
            hint.value = "打不开相机：" + e.message
        }
    }

    private val deviceCallback = object : CameraDevice.StateCallback() {
        override fun onOpened(dev: CameraDevice) {
            camera = dev
            startSession()
        }

        override fun onDisconnected(dev: CameraDevice) {
            dev.close()
            camera = null
        }

        override fun onError(dev: CameraDevice, error: Int) {
            dev.close()
            camera = null
            runOnUiThread { hint.value = "相机出错（$error），请返回重试" }
        }
    }

    private fun startSession() {
        val dev = camera ?: return
        val st = preview.surfaceTexture ?: return
        try {
            val ch = (getSystemService(CAMERA_SERVICE) as CameraManager)
                .getCameraCharacteristics(cameraId!!)
            val map = ch.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
            if (map == null) {
                hint.value = "相机能力读取失败"
                return
            }
            val pv = nearest(map.getOutputSizes(SurfaceTexture::class.java), TARGET_W, TARGET_H)
            val dec = nearest(map.getOutputSizes(ImageFormat.YUV_420_888), TARGET_W, TARGET_H)
            if (pv == null || dec == null) {
                hint.value = "相机不支持所需的预览格式"
                return
            }
            st.setDefaultBufferSize(pv.width, pv.height)
            val previewSurface = Surface(st)
            val r = ImageReader.newInstance(dec.width, dec.height, ImageFormat.YUV_420_888, 2)
            reader = r
            r.setOnImageAvailableListener({ onFrame(it) }, bg)

            val outputs = listOf(previewSurface, r.surface)
            dev.createCaptureSession(
                outputs,
                object : CameraCaptureSession.StateCallback() {
                    override fun onConfigured(s: CameraCaptureSession) {
                        session = s
                        try {
                            val b = dev.createCaptureRequest(CameraDevice.TEMPLATE_PREVIEW)
                            b.addTarget(previewSurface)
                            b.addTarget(r.surface)
                            // 连续对焦：近距离扫屏时固定焦距经常糊到解不出来
                            b.set(
                                CaptureRequest.CONTROL_AF_MODE,
                                CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_PICTURE,
                            )
                            s.setRepeatingRequest(b.build(), null, bg)
                        } catch (e: Exception) {
                            runOnUiThread { hint.value = "启动预览失败：" + e.message }
                        }
                    }

                    override fun onConfigureFailed(s: CameraCaptureSession) {
                        runOnUiThread { hint.value = "相机预览配置失败，请返回重试" }
                    }
                },
                bg,
            )
        } catch (e: Exception) {
            hint.value = "启动相机失败：" + e.message
        }
    }

    /** 取长宽比最接近目标、且不超过目标像素量的那一档，避免 4K 预览白烧 CPU。 */
    private fun nearest(sizes: Array<Size>?, w: Int, h: Int): Size? {
        if (sizes == null || sizes.isEmpty()) {
            return null
        }
        var best = sizes[0]
        var bestScore = Long.MAX_VALUE
        for (s in sizes) {
            val score = Math.abs(s.width.toLong() - w) + Math.abs(s.height.toLong() - h)
            if (score < bestScore) {
                bestScore = score
                best = s
            }
        }
        return best
    }

    private fun onFrame(r: ImageReader) {
        val img = try {
            r.acquireLatestImage()
        } catch (e: IllegalStateException) {
            null
        } ?: return
        // acquire 之后必须 close，否则 ImageReader 很快就再也拿不到帧
        try {
            if (done || !busy.compareAndSet(false, true)) {
                return
            }
            val text = decode(img)
            if (text != null) {
                finishWith(text)
            }
        } finally {
            img.close()
            busy.set(false)
        }
    }

    /** 只取 Y 平面做灰度解码：QR 识别不需要彩色，省掉一次 YUV→RGB 转换。 */
    private fun decode(img: Image): String? {
        return try {
            val plane = img.planes[0]
            val buf = plane.buffer
            val w = img.width
            val h = img.height
            val rowStride = plane.rowStride
            val pixelStride = plane.pixelStride
            val data = ByteArray(w * h)
            if (pixelStride == 1 && rowStride == w) {
                buf.get(data, 0, w * h)
            } else {
                // 有的机型每行末尾有对齐填充，必须按 rowStride 逐行取，否则整幅图会斜切
                val row = ByteArray(rowStride)
                for (y in 0 until h) {
                    buf.position(y * rowStride)
                    val n = minOf(rowStride, buf.remaining())
                    buf.get(row, 0, n)
                    for (x in 0 until w) {
                        data[y * w + x] = row[x * pixelStride]
                    }
                }
            }
            val src = PlanarYUVLuminanceSource(data, w, h, 0, 0, w, h, false)
            val bitmap = BinaryBitmap(HybridBinarizer(src))
            // 相册解码在另一条线程上用同一个 reader，必须互斥，否则 hints/状态会串
            synchronized(qr) {
                try {
                    qr.decodeWithState(bitmap)?.text
                } finally {
                    qr.reset()
                }
            }
        } catch (e: NotFoundException) {
            null
        } catch (e: ArrayIndexOutOfBoundsException) {
            null
        }
    }

    private fun finishWith(text: String) {
        if (done) {
            return
        }
        done = true
        runOnUiThread {
            setResult(RESULT_OK, Intent().putExtra(EXTRA_RESULT, text))
            finish()
        }
    }

    /** 从系统文件选择器选一张图；不需要相机或存储权限，相机被拒时也能用。 */
    private fun openGallery() {
        val i = Intent(Intent.ACTION_GET_CONTENT)
        i.type = "image/*"
        i.addCategory(Intent.CATEGORY_OPENABLE)
        try {
            startActivityForResult(Intent.createChooser(i, "选择二维码图片"), REQ_GALLERY)
        } catch (e: Exception) {
            hint.value = "打不开相册：" + e.message
        }
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_GALLERY) {
            return
        }
        if (resultCode != RESULT_OK || data?.data == null) {
            return
        }
        val uri = data.data ?: return
        hint.value = "正在识别图片…"
        // 解码是纯 CPU 活，放后台线程，避免大图把界面卡成假死
        Thread { decodeImageFromUri(uri) }.start()
    }

    /**
     * 解码相册图片里的二维码，成功走与相机扫码完全相同的回填路径。
     * 先按长边降采样：手机原图动辄 4000px，全尺寸解码既慢又容易 OOM。
     */
    private fun decodeImageFromUri(uri: Uri) {
        var bmp: Bitmap? = null
        try {
            val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
            contentResolver.openInputStream(uri)?.use {
                BitmapFactory.decodeStream(it, null, bounds)
            }
            var sample = 1
            while (bounds.outWidth / sample > IMAGE_MAX_EDGE
                || bounds.outHeight / sample > IMAGE_MAX_EDGE) {
                sample *= 2
            }
            val opt = BitmapFactory.Options().apply { inSampleSize = sample }
            bmp = contentResolver.openInputStream(uri)?.use {
                BitmapFactory.decodeStream(it, null, opt)
            }
            if (bmp == null) {
                failImage("这张图片读不出来，换一张试试")
                return
            }
            val w = bmp.width
            val h = bmp.height
            val pixels = IntArray(w * h)
            bmp.getPixels(pixels, 0, w, 0, 0, w, h)
            val bitmap = BinaryBitmap(
                HybridBinarizer(RGBLuminanceSource(w, h, pixels)),
            )
            val result: Result
            synchronized(qrImageReader) {
                result = qrImageReader.decode(bitmap, imageHints)
            }
            finishWith(result.text)
        } catch (e: NotFoundException) {
            // 连定位图案都没找到：多半不是二维码，也可能整张糊到没有边缘
            failImage("图片里没找到二维码\n换一张更清晰的图，或直接用相机扫")
        } catch (e: ChecksumException) {
            // 定位到了但内容校验不过：典型是拍糊、过度缩放或被裁掉一角
            failImage("二维码识别失败（图片模糊或被裁切）\n换一张清晰的图再试")
        } catch (e: FormatException) {
            failImage("二维码识别失败（图片模糊或被裁切）\n换一张清晰的图再试")
        } catch (e: Exception) {
            failImage("识别失败：" + e.message)
        } finally {
            bmp?.recycle()
        }
    }

    private fun failImage(msg: String) {
        runOnUiThread { hint.value = msg }
    }

    override fun onResume() {
        super.onResume()
        // 首次进来要先弹相机授权，授权弹窗会把 Activity 暂停、相机会被关掉；
        // 授权通过或从后台回来时这里把相机重新拉起来，否则画面会一直是黑的
        if (preview.isAvailable && camera == null) {
            openCamera()
        }
    }

    override fun onPause() {
        closeCamera()
        super.onPause()
    }

    override fun onDestroy() {
        closeCamera()
        bgThread?.quitSafely()
        bgThread = null
        super.onDestroy()
    }

    private fun closeCamera() {
        try {
            session?.close()
        } catch (e: Exception) {
        }
        session = null
        try {
            camera?.close()
        } catch (e: Exception) {
        }
        camera = null
        try {
            reader?.close()
        } catch (e: Exception) {
        }
        reader = null
    }
}
