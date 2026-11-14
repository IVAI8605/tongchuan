package com.tongchuan;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

import org.json.JSONObject;

/**
 * v0.8 剪贴板同步（手机侧）：常驻前台服务，按固定间隔从对端（当前发送目标）拉最新一条，
 * 有新内容就写进系统剪贴板——电脑复制、手机粘贴。
 *
 * 方向说明（功能展望 §3.2 的结论）：**电脑 → 手机可以自动**；手机 → 电脑做不到后台自动读
 * （Android 10 起后台读剪贴板被系统拦），保底是「点一下」——磁贴或通知把界面拉到前台再读再推。
 * 所以本服务只负责「拉 + 写」，推送入口在 MainActivity。
 *
 * 每次拉取新建一条 TLS 连接：剪贴板消息量小，先用短连接把功能做稳；
 * 若实测延迟明显再改成长连接（内核已支持，只需把连接挂在服务里）。
 */
public class ClipService extends Service {

    private static final String TAG = "通传";
    private static final String CHANNEL_ID = "clip";
    private static final int NOTIFICATION_ID = 1002;
    /** 拉取间隔：太短会不停握手，太长会让「电脑复制 → 手机可用」的体感变差 */
    private static final long POLL_MS = 1500;
    public static final String ACTION_STOP = "com.tongchuan.CLIP_STOP";

    private static volatile ClipService alive;
    private static volatile String lastStatus = "未启动";

    private volatile boolean running = true;
    private volatile long since = 0;

    public static boolean isRunning() {
        return alive != null;
    }

    public static String lastStatus() {
        return lastStatus;
    }

    public static void start(Context ctx) {
        Intent i = new Intent(ctx, ClipService.class);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            ctx.startForegroundService(i);
        } else {
            ctx.startService(i);
        }
    }

    public static void stop(Context ctx) {
        Intent i = new Intent(ctx, ClipService.class);
        i.setAction(ACTION_STOP);
        ctx.startService(i);
    }

    @Override
    public void onCreate() {
        super.onCreate();
        alive = this;
        createChannel();
        startForeground(NOTIFICATION_ID, buildNotification("正在连接…"));
        new Thread(this::loop, "tc-clip").start();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && ACTION_STOP.equals(intent.getAction())) {
            stopSelf();
            return START_NOT_STICKY;
        }
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
        running = false;
        alive = null;
        lastStatus = "未启动";
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    private void loop() {
        while (running) {
            String addr = clipTarget(this);
            String fp = pref("send_fp");
            String name = pref("send_name");
            if (addr.isEmpty() || fp.isEmpty()) {
                setStatus("还没选对端：先在上面「发现附近的设备」里选一台");
                sleep(POLL_MS * 2);
                continue;
            }
            String identityDir = ReceiverService.identityDir(this).getAbsolutePath();
            String out = TcNative.clipPullJson(identityDir, addr, fp, since, Build.MODEL);
            if (out == null) {
                setStatus("接口不可用");
                sleep(POLL_MS * 2);
                continue;
            }
            try {
                JSONObject o = new JSONObject(out);
                if (!o.optBoolean("ok")) {
                    setStatus("连接失败：" + o.optString("reason", "未知原因"));
                } else if (o.optBoolean("has")) {
                    since = o.optLong("lamport", since);
                    String text = o.optString("text", "");
                    boolean sensitive = o.optBoolean("sensitive", false);
                    if (!text.isEmpty() && !sensitive) {
                        writeClipboard(text);
                        setStatus("已同步来自「" + who(name, addr) + "」的内容");
                    } else if (sensitive) {
                        // 带敏感标记的不自动写回：密码类内容不该悄悄进剪贴板
                        setStatus("收到一条带敏感标记的内容，已跳过");
                    }
                } else {
                    setStatus("已连接「" + who(name, addr) + "」，等待新内容");
                }
            } catch (Exception e) {
                setStatus("解析失败：" + e.getClass().getSimpleName());
            }
            sleep(POLL_MS);
        }
    }

    private static String who(String name, String addr) {
        return name == null || name.isEmpty() ? addr : name;
    }

    /**
     * 剪贴板对端地址：主机沿用发送目标，端口另填（服务端 clip_listen 与文件端口是两个）。
     * 发现报文只带文件端口，所以这里不做推断，让用户显式填一个端口，配错时错误信息也直白。
     */
    static String clipTarget(Context ctx) {
        String send = ctx.getSharedPreferences(Dest.PREFS, MODE_PRIVATE)
                .getString("send_addr", "");
        String port = ctx.getSharedPreferences(Dest.PREFS, MODE_PRIVATE)
                .getString("clip_port", "53320");
        if (send.isEmpty()) {
            return "";
        }
        String host = send.contains(":") ? send.substring(0, send.lastIndexOf(':')) : send;
        if (port == null || port.trim().isEmpty()) {
            port = "53320";
        }
        return host + ":" + port.trim();
    }

    private void sleep(long ms) {
        try {
            Thread.sleep(ms);
        } catch (InterruptedException e) {
            running = false;
        }
    }

    /** 写系统剪贴板。Android 只限制「后台读」，写不受限，所以拉取方向可以全自动。 */
    private void writeClipboard(String text) {
        try {
            ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (cm != null) {
                cm.setPrimaryClip(ClipData.newPlainText("通传", text));
            }
        } catch (Exception e) {
            Log.w(TAG, "写剪贴板失败", e);
        }
    }

    private void setStatus(String s) {
        lastStatus = s;
        // 状态里只有设备名与原因，没有剪贴板内容，可以直接进日志
        Log.i("TONGCHUAN-CLIP", s);
        updateNotification(s);
    }

    private void createChannel() {
        NotificationManager nm = getSystemService(NotificationManager.class);
        NotificationChannel ch = new NotificationChannel(
                CHANNEL_ID, "剪贴板同步", NotificationManager.IMPORTANCE_LOW);
        ch.setShowBadge(false);
        nm.createNotificationChannel(ch);
    }

    private Notification buildNotification(String text) {
        Intent open = new Intent(this, MainActivity.class);
        PendingIntent pi = PendingIntent.getActivity(
                this, 0, open, PendingIntent.FLAG_IMMUTABLE);
        // 「推送剪贴板」必须把界面拉到前台：后台读剪贴板会被系统拦成空内容
        Intent push = new Intent(this, MainActivity.class).putExtra("clip_push", true);
        PendingIntent pp = PendingIntent.getActivity(this, 1, push,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Builder(this, CHANNEL_ID)
                .setContentTitle("通传 · 剪贴板同步")
                .setContentText(text)
                .setSmallIcon(R.drawable.ic_launcher)
                .setContentIntent(pi)
                .setOngoing(true)
                .addAction(new Notification.Action.Builder(null, "推送剪贴板", pp).build())
                .build();
    }

    private void updateNotification(String text) {
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.notify(NOTIFICATION_ID, buildNotification(text));
    }

    private String pref(String key) {
        return getSharedPreferences(Dest.PREFS, MODE_PRIVATE).getString(key, "");
    }
}
