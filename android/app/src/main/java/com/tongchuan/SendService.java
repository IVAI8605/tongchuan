package com.tongchuan;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;

import org.json.JSONObject;

/**
 * 发送前台服务（M5）。之前发送跑在 Activity 的线程里，界面一退到后台就随时可能被回收，
 * 熄屏十分钟基本必断；改成前台服务 + 部分唤醒锁，让传输按预期跑完。
 *
 * 与接收服务分开是因为生命周期不同：接收是常驻，发送是一次性的，结束就该消失，
 * 也不该复用同一个通知 id（否则会把「正在等待接收」的通知顶掉）。
 */
public class SendService extends Service {
    private static final String CHANNEL_ID = "send";
    private static final int NOTICE_ID = 0x7C51;
    private static final long RETRY_DELAY_MS = 1200;

    /** 结果只落在进程内静态字段：Activity 与它同进程，轮询取走即可，不必再引广播或绑定。 */
    private static volatile String lastResult;
    private static volatile boolean running;
    /** v0.11：暂停标记，只用于通知文案；真正的暂停在 Rust 侧的控制位里生效 */
    static volatile boolean paused;

    public static boolean isRunning() {
        return running;
    }

    /** 取走结果（取一次就清空），没有结果时返回 null。 */
    public static String takeResult() {
        String r = lastResult;
        lastResult = null;
        return r;
    }

    public static void start(Context ctx, String target, String fp, String pathsJson,
                             String pairCode, String identityDir, String deviceName, boolean force,
                             String destId) {
        Intent i = new Intent(ctx, SendService.class);
        i.putExtra("target", target);
        i.putExtra("fp", fp);
        i.putExtra("paths", pathsJson);
        i.putExtra("pair", pairCode);
        i.putExtra("identity", identityDir);
        i.putExtra("device", deviceName);
        // v0.12：强制重传随 Intent 传给后台线程，界面与通知都不需要再读 UI 状态
        i.putExtra("force", force);
        // v0.16：接收端落点标识，空串 = 默认落点
        i.putExtra("destId", destId == null ? "" : destId);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            ctx.startForegroundService(i);
        } else {
            ctx.startService(i);
        }
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    @Override
    public void onCreate() {
        super.onCreate();
        NotificationManager nm = getSystemService(NotificationManager.class);
        NotificationChannel ch = new NotificationChannel(
                CHANNEL_ID, "发送文件", NotificationManager.IMPORTANCE_LOW);
        ch.setShowBadge(false);
        nm.createNotificationChannel(ch);
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent == null) {
            stopSelf();
            return START_NOT_STICKY;
        }
        if (running) {
            // 同一时间只允许一批：两批并发会把进度快照互相覆盖，界面也说不清在传哪个
            return START_NOT_STICKY;
        }
        final String target = intent.getStringExtra("target");
        final String fp = intent.getStringExtra("fp");
        final String paths = intent.getStringExtra("paths");
        final String pair = intent.getStringExtra("pair");
        final String identity = intent.getStringExtra("identity");
        final String device = intent.getStringExtra("device");
        final boolean force = intent.getBooleanExtra("force", false);
        // v0.16：接收端落点标识；空串表示用对方默认落点
        final String destId = intent.getStringExtra("destId");

        running = true;
        lastResult = null;
        startForeground(NOTICE_ID, build("正在发送…"));

        new Thread(() -> {
            PowerManager pm = (PowerManager) getSystemService(Context.POWER_SERVICE);
            PowerManager.WakeLock lock = pm.newWakeLock(
                    PowerManager.PARTIAL_WAKE_LOCK, "tongchuan:send");
            lock.acquire(6 * 60 * 60 * 1000L); // 上限 6 小时，异常退出也不会一直亮着

            // 进度单独一个线程刷通知：发送本身是阻塞到会话结束的
            final boolean[] alive = {true};
            Thread ui = new Thread(() -> {
                while (alive[0]) {
                    try {
                        update(progressText());
                    } catch (Exception ignored) {
                    }
                    try {
                        Thread.sleep(700);
                    } catch (InterruptedException e) {
                        return;
                    }
                }
            }, "tc-send-notice");
            ui.start();

            String out = null;
            try {
                out = TcNative.sendJson(target, fp, paths, pair == null ? "" : pair,
                        identity, device, force, destId == null ? "" : destId);
                // 网络类失败自动重试一次：多半是刚锁屏/刚切信道导致的瞬断，重试成本低
                if (isRetryable(out)) {
                    Thread.sleep(RETRY_DELAY_MS);
                    out = TcNative.sendJson(target, fp, paths, pair == null ? "" : pair,
                            identity, device, force, destId == null ? "" : destId);
                }
            } catch (Exception e) {
                out = "{\"ok\":false,\"reason\":\"" + e.getMessage() + "\"}";
            } finally {
                alive[0] = false;
                lock.release();
            }

            lastResult = out;
            running = false;
            stopForeground(STOP_FOREGROUND_REMOVE);
            stopSelf();
        }, "tc-send-worker").start();
        return START_NOT_STICKY;
    }

    /** 只对「网络瞬断」重试，其余（未信任、版本不符、配对码错）重试也没用，只会让用户多等。 */
    private static boolean isRetryable(String out) {
        if (out == null) {
            return true;
        }
        try {
            JSONObject o = new JSONObject(out);
            if (o.optBoolean("ok")) {
                return false;
            }
            String r = o.optString("reason", "").toLowerCase();
            return r.contains("os error") || r.contains("timed out")
                    || r.contains("connection") || r.contains("reset");
        } catch (Exception e) {
            return false;
        }
    }

    private String progressText() {
        String p = TcNative.sendProgress();
        if (p == null) {
            return paused ? "已暂停" : "正在发送…";
        }
        try {
            JSONObject o = new JSONObject(p);
            if (!o.optBoolean("active")) {
                return "正在收尾…";
            }
            String head = paused ? "已暂停 · " : "";
            if ("scanning".equals(o.optString("phase", "sending"))) {
                return head + "正在扫描：已处理 " + o.optLong("scanFiles") + " 个文件 / "
                        + human(o.optLong("scanBytes"));
            }
            return head + "已发送 " + o.optInt("percent", 0) + "%  "
                    + human(o.optLong("bytesSent")) + " / " + human(o.optLong("totalBytes"))
                    + "  " + human(o.optLong("speedBps")) + "/s";
        } catch (Exception e) {
            return paused ? "已暂停" : "正在发送…";
        }
    }

    private Notification build(String text) {
        Intent open = new Intent(this, MainActivity.class);
        PendingIntent pi = PendingIntent.getActivity(
                this, 0, open, PendingIntent.FLAG_IMMUTABLE);
        return new Notification.Builder(this, CHANNEL_ID)
                .setContentTitle("通传 · 发送")
                .setContentText(text)
                .setSmallIcon(R.drawable.ic_launcher)
                .setContentIntent(pi)
                .setOngoing(true)
                .build();
    }

    private void update(String text) {
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.notify(NOTICE_ID, build(text));
    }

    private static String human(long n) {
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
}
