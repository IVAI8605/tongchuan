package com.tongchuan;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.Build;
import android.os.Environment;
import android.os.FileObserver;
import android.util.Log;

import org.json.JSONArray;

import java.io.File;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.List;
import java.util.Set;

/**
 * 截屏自动发送：手机相册的 Screenshots 目录出现新截图时，按当前「发送到」目标
 * 用现有文件通道发给电脑；电脑端收到后由 Windows 剪贴板桥把图片放进剪贴板。
 *
 * 不读系统剪贴板：Android 10+ 后台读剪贴板会被系统拦截；截屏文件监测不需要这个权限，
 * 而且截图是独立的图片文件，用文件通道比把大图塞进 64 KiB 的剪贴板正文协议更稳。
 */
public final class ScreenshotWatcher {
    private static final String TAG = "TONGCHUAN-SHOT";
    private static final Set<String> seen = Collections.synchronizedSet(new HashSet<String>());
    private static final List<FileObserver> observers = new ArrayList<>();
    private static long startedAt = 0;

    private ScreenshotWatcher() {
    }

    public static synchronized void start(Context ctx) {
        stop();
        startedAt = System.currentTimeMillis();
        List<File> dirs = screenshotDirs();
        for (File dir : dirs) {
            if (!dir.isDirectory()) {
                continue;
            }
            final File watched = dir;
            FileObserver observer = new FileObserver(watched.getAbsolutePath(),
                    FileObserver.CREATE | FileObserver.CLOSE_WRITE | FileObserver.MOVED_TO) {
                @Override
                public void onEvent(int event, String path) {
                    if (path == null || path.isEmpty()) {
                        return;
                    }
                    File file = new File(watched, path);
                    if (!isScreenshotFile(file)) {
                        return;
                    }
                    // 截图写入可能分几个事件，等文件稳定后再发；按绝对路径去重，
                    // 避免同时监听 Screenshots 和它的父目录时把同一张图发两遍。
                    final String key = file.getAbsolutePath();
                    if (!seen.add(key)) {
                        return;
                    }
                    new Thread(() -> {
                        sleep(700);
                        if (!file.isFile() || file.lastModified() < startedAt - 1000) {
                            return;
                        }
                        send(ctx, file);
                    }, "tc-shot-send").start();
                }
            };
            observer.startWatching();
            observers.add(observer);
            Log.i(TAG, "开始监听截图目录 " + watched);
        }
    }

    public static synchronized void stop() {
        for (FileObserver observer : observers) {
            try {
                observer.stopWatching();
            } catch (Exception ignored) {
            }
        }
        observers.clear();
        seen.clear();
    }

    /** 文件名过滤：只认 Screenshot*.png/jpg/jpeg，避免把普通相册图片全发走。 */
    static boolean isScreenshotFile(File file) {
        if (file == null || !file.isFile()) {
            return false;
        }
        String name = file.getName().toLowerCase();
        if (!name.startsWith("screenshot")) {
            return false;
        }
        return name.endsWith(".png") || name.endsWith(".jpg")
                || name.endsWith(".jpeg") || name.endsWith(".webp");
    }

    private static List<File> screenshotDirs() {
        List<File> out = new ArrayList<>();
        File pictures = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_PICTURES);
        File dcim = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DCIM);
        out.add(new File(pictures, "Screenshots"));
        out.add(new File(dcim, "Screenshots"));
        // 部分 OEM 会把截图放在 Pictures 根下；父目录也监听一下，文件名过滤仍然生效。
        out.add(pictures);
        out.add(dcim);
        return out;
    }

    private static void send(Context ctx, File file) {
        SharedPreferences p = ctx.getSharedPreferences(Dest.PREFS, Context.MODE_PRIVATE);
        String addr = p.getString("send_addr", "");
        String fp = p.getString("send_fp", "");
        if (addr.isEmpty() || fp.isEmpty()) {
            Log.w(TAG, "截图未发送：还没有选择发送目标");
            return;
        }
        try {
            JSONArray paths = new JSONArray();
            paths.put(file.getAbsolutePath());
            SendService.start(ctx, addr, fp, paths.toString(), "",
                    ReceiverService.identityDir(ctx).getAbsolutePath(), Build.MODEL, false, "");
            Log.i(TAG, "截图已送入发送队列 " + file.getName());
        } catch (Exception e) {
            Log.w(TAG, "截图发送启动失败", e);
        }
    }

    private static void sleep(long ms) {
        try {
            Thread.sleep(ms);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }
}
