package com.tongchuan;

import android.media.RingtoneManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.net.Uri;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * 常驻接收服务。用 connectedDevice 类型的前台服务，避免 dataSync 在 Android 15 上
 * 「24 小时累计 6 小时」的硬上限；通知常驻但可以被系统折叠。
 * 接收落点先用应用外部私有目录（不需要任何授权、也不会混进相册），
 * 让用户可以自己选目录的 SAF 方案在下一步接。
 */
public class ReceiverService extends Service {

    private static final String TAG = "通传";
    private static final String CHANNEL_ID = "receiver";
    private static final String CHANNEL_ARRIVED = "arrived";
    private static final int NOTIFICATION_ID = 1001;
    private static final int ARRIVED_ID_BASE = 2000;
    private static final AtomicInteger arrivedSeq = new AtomicInteger(0);
    public static final String ACTION_STOP = "com.tongchuan.STOP";
    public static final String ACTION_RESTART = "com.tongchuan.RESTART";

    private static volatile ReceiverService alive;
    private static volatile String lastError = "";

    private long handle = -1;
    /** 常驻期间持有的两把保活锁；见 acquireKeepAliveLocks 里的取舍说明 */
    private WifiManager.WifiLock wifiLock;
    private PowerManager.WakeLock cpuLock;

    public static boolean isRunning() {
        return alive != null;
    }

    public static String lastError() {
        return lastError;
    }

    /**
     * 身份与信任库共用一个目录（identity.cer/key + trust.jsonl）。
     * 接收线程里的门禁和界面的「待确认/已信任」必须读同一份，否则批了也放不进来。
     */
    public static File identityDir(Context ctx) {
        return new File(ctx.getFilesDir(), "identity");
    }

    public static void start(Context ctx) {
        Intent i = new Intent(ctx, ReceiverService.class);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            ctx.startForegroundService(i);
        } else {
            ctx.startService(i);
        }
    }

    /** 落点改了要换配置重开接收端；走 startService 重入，避免 stop 后立刻 start 的竞态。 */
    public static void restart(Context ctx) {
        Intent i = new Intent(ctx, ReceiverService.class);
        i.setAction(ACTION_RESTART);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            ctx.startForegroundService(i);
        } else {
            ctx.startService(i);
        }
    }

    @Override
    public void onCreate() {
        super.onCreate();
        alive = this;
        createChannel();
        startForeground(NOTIFICATION_ID, buildNotification("正在等待接收"));
        acquireKeepAliveLocks();
        launchReceiver();
        // v0.21：常驻接收时同时监听截图目录，新截图自动走文件通道发给电脑
        ScreenshotWatcher.start(this);
    }

    /**
     * 实测：锁屏静置 30 分钟进入 Doze 后，电脑投递直接连接超时，文件送不进来。
     * 原因是深睡时系统把 Wi-Fi 挂起、并把设备切进 IDLE，监听端口对外不可达。
     *
     * WifiLock 让网卡不做省电休眠，PARTIAL_WAKE_LOCK 让设备不进入 Doze——
     * 只有两者同时持有才能保证「随时能收到」。代价是常驻耗电会上升，
     * 这是文件接收工具必须付的成本；停止接收时立即释放，不长期占用。
     */
    private void acquireKeepAliveLocks() {
        try {
            WifiManager wm = (WifiManager) getApplicationContext()
                    .getSystemService(Context.WIFI_SERVICE);
            if (wm != null) {
                wifiLock = wm.createWifiLock(
                        WifiManager.WIFI_MODE_FULL_HIGH_PERF, "tongchuan:receiver");
                // 不做引用计数：服务只获取一次，避免多取少放导致锁泄漏
                wifiLock.setReferenceCounted(false);
                wifiLock.acquire();
            }
        } catch (Exception e) {
            // 拿不到锁不能影响接收本身，退化成原来的行为并留日志
            Log.e(TAG, "WifiLock 获取失败", e);
            wifiLock = null;
        }
        try {
            PowerManager pm = (PowerManager) getSystemService(Context.POWER_SERVICE);
            if (pm != null) {
                cpuLock = pm.newWakeLock(
                        PowerManager.PARTIAL_WAKE_LOCK, "tongchuan:receiver");
                cpuLock.setReferenceCounted(false);
                cpuLock.acquire();
            }
        } catch (Exception e) {
            Log.e(TAG, "PARTIAL_WAKE_LOCK 获取失败", e);
            cpuLock = null;
        }
    }

    private void releaseKeepAliveLocks() {
        if (cpuLock != null) {
            if (cpuLock.isHeld()) {
                cpuLock.release();
            }
            cpuLock = null;
        }
        if (wifiLock != null) {
            if (wifiLock.isHeld()) {
                wifiLock.release();
            }
            wifiLock = null;
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && ACTION_STOP.equals(intent.getAction())) {
            stopSelf();
            return START_NOT_STICKY;
        }
        if (intent != null && ACTION_RESTART.equals(intent.getAction())) {
            launchReceiver();
            return START_STICKY;
        }
        // 被系统回收后重新拉起时，说明我们本来就该常驻
        return START_STICKY;
    }

    /** 起（或重起）原生接收端，配置每次重新拼，保证落点改动立即生效。 */
    private void launchReceiver() {
        if (handle >= 0) {
            TcNative.stopReceiver(handle);
            handle = -1;
        }
        handle = TcNative.startReceiver(buildConfig().toString());
        if (handle < 0) {
            lastError = "启动失败：配置非法或端口 53318 被占用";
            Log.e(TAG, lastError);
            updateNotification(lastError);
        } else {
            lastError = "";
            updateNotification("正在等待接收");
        }
    }

    @Override
    public void onDestroy() {
        ScreenshotWatcher.stop();
        if (handle >= 0) {
            TcNative.stopReceiver(handle);
            handle = -1;
        }
        // 用户停止接收或系统回收服务时必须放锁，否则会一直拖着网卡和 CPU
        releaseKeepAliveLocks();
        alive = null;
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private JSONObject buildConfig() {
        File dest = Dest.current(this);
        JSONObject o = new JSONObject();
        try {
            o.put("alias", "手机-" + Build.MODEL);
            o.put("listen", "0.0.0.0:53318");
            o.put("manage", "127.0.0.1:53319");
            o.put("destination", dest.getAbsolutePath());
            o.put("identity_dir", identityDir(this).getAbsolutePath());
            o.put("log_dir", new File(getFilesDir(), "logs").getAbsolutePath());
            o.put("queue_dir", new File(getFilesDir(), "queue").getAbsolutePath());
            o.put("log_days", 2);
            o.put("auto_accept", true);
            JSONArray allow = new JSONArray();
            allow.put(dest.getAbsolutePath());
            o.put("allow", allow);
        } catch (Exception e) {
            Log.e(TAG, "拼配置失败", e);
        }
        return o;
    }

    /**
     * Rust 接收线程在会话结束时回调（不是 Java 线程，所以 JNI 那边会先 attach 到 JVM）。
     * 用事件回调而不是轮询状态，是为了不让常驻服务为了「有没有新文件」周期性唤醒 CPU。
     * v0.12：infoJson 带跳过数、落点根目录与来源设备，通知与「最近接收」共用这一份口径。
     */
    public static void onTransfer(long files, long bytes, boolean interrupted, boolean verified,
                                  String infoJson) {
        ReceiverService s = alive;
        if (s == null) {
            return; // 服务已停，收尾通知无处可发，静默忽略
        }
        s.showArrived(files, bytes, interrupted, verified, infoJson);
    }

    /** 另开一条渠道：常驻通知是 LOW（不响），送达通知要 DEFAULT（响一下但不能亮屏）。 */
    private void showArrived(long files, long bytes, boolean interrupted, boolean verified,
                             String infoJson) {
        long skipped = 0;
        String device = "";
        StringBuilder roots = new StringBuilder();
        try {
            JSONObject o = new JSONObject(infoJson == null ? "{}" : infoJson);
            skipped = o.optLong("skipped");
            device = o.optString("device", "");
            JSONArray arr = o.optJSONArray("roots");
            if (arr != null) {
                for (int i = 0; i < arr.length(); i++) {
                    if (i > 0) {
                        roots.append("、");
                    }
                    roots.append(arr.optString(i));
                }
            }
        } catch (Exception ignored) {
            // 解析失败不影响通知：少几项说明也比不响强
        }

        NotificationManager nm = getSystemService(NotificationManager.class);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            NotificationChannel ch = nm.getNotificationChannel(CHANNEL_ARRIVED);
            if (ch == null) {
                ch = new NotificationChannel(
                        CHANNEL_ARRIVED, "收到文件", NotificationManager.IMPORTANCE_DEFAULT);
                // 重要性一旦创建就不能由应用改，这里一次定型：会响一声、不弹横幅、不亮屏
                ch.setDescription("文件送达时响一下，不亮屏");
                ch.enableVibration(false);
                Uri sound = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_NOTIFICATION);
                ch.setSound(sound, new android.media.AudioAttributes.Builder()
                        .setUsage(android.media.AudioAttributes.USAGE_NOTIFICATION)
                        .setContentType(android.media.AudioAttributes.CONTENT_TYPE_SONIFICATION)
                        .build());
                nm.createNotificationChannel(ch);
            }
        }

        String title = interrupted ? "通传：传输中断，可续传" : "通传：已收到 " + files + " 个文件";
        StringBuilder text = new StringBuilder();
        text.append(interrupted
                ? "已收 " + humanSize(bytes) + "，下次会自动从断点继续"
                : humanSize(bytes) + (verified ? " · 校验通过" : " · 校验未通过"));
        // v0.12：跳过数与发送侧同口径，用户能看出「这次实际新收了几个」
        if (skipped > 0) {
            text.append(" · 跳过 ").append(skipped).append(" 个");
        }
        if (roots.length() > 0) {
            text.append(" · 存入 ").append(roots);
        }
        if (!device.isEmpty()) {
            text.append(" · 来自 ").append(device);
        }
        // 「最近接收」展开页要看同一份结果：落点是相对接收目录的根目录名，不写具体文件
        getSharedPreferences(Dest.PREFS, MODE_PRIVATE).edit()
                .putString("last_recv", device + "|" + roots + "|" + skipped + "|"
                        + files + "|" + bytes + "|" + (verified ? "1" : "0")
                        + "|" + (interrupted ? "1" : "0"))
                .apply();

        Intent open = new Intent(this, MainActivity.class);
        PendingIntent pi = PendingIntent.getActivity(this, ARRIVED_ID_BASE, open,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification n = new Notification.Builder(this,
                Build.VERSION.SDK_INT >= Build.VERSION_CODES.O ? CHANNEL_ARRIVED : CHANNEL_ID)
                .setContentTitle(title)
                .setContentText(text.toString())
                .setSmallIcon(R.drawable.ic_launcher)
                .setContentIntent(pi)
                .setAutoCancel(true)
                .build();
        // 每条送达用自己的 ID：连投多次能各自响一下，且点开一条不会把其它条一起消掉
        nm.notify(ARRIVED_ID_BASE + arrivedSeq.incrementAndGet(), n);
    }

    private static String humanSize(long n) {
        if (n < 1024) {
            return n + " B";
        }
        if (n < 1024L * 1024) {
            return String.format("%.1f KB", n / 1024.0);
        }
        if (n < 1024L * 1024 * 1024) {
            return String.format("%.1f MB", n / 1048576.0);
        }
        return String.format("%.2f GB", n / 1073741824.0);
    }

    private void createChannel() {
        NotificationManager nm = getSystemService(NotificationManager.class);
        NotificationChannel ch = new NotificationChannel(
                CHANNEL_ID, "接收服务", NotificationManager.IMPORTANCE_LOW);
        ch.setShowBadge(false);
        nm.createNotificationChannel(ch);
    }

    private Notification buildNotification(String text) {
        Intent open = new Intent(this, MainActivity.class);
        PendingIntent pi = PendingIntent.getActivity(
                this, 0, open, PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Builder(this, CHANNEL_ID)
                .setContentTitle("通传")
                .setContentText(text)
                .setSmallIcon(R.drawable.ic_launcher)
                .setContentIntent(pi)
                .setOngoing(true)
                .build();
    }

    private void updateNotification(String text) {
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.notify(NOTIFICATION_ID, buildNotification(text));
    }
}
