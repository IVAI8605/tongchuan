package com.tongchuan;

import android.content.Context;
import android.os.Environment;
import android.util.Log;

import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;

/**
 * v1.1.0：手机侧操作记录（追加写的 JSONL），供事后经 ADB 取回。
 *
 * 为什么另起一份而不靠 logcat：logcat 是 4 MiB 环形缓冲，几十分钟就滚没了，
 * 事后再想查「上次那次发送为什么没记录」时早已找不到。这里落文件，随用随取。
 *
 * 只记动作、计数、耗时、结果；**不记文件路径、文件名、内容、指纹/令牌原文**（规范 #14）。
 * 文件在应用私有目录 files/ops-log.jsonl；单文件超 4 MiB 就改名归档（只改名不删）。
 */
final class OpsLog {

    private static final String FILE = "ops-log.jsonl";
    /** 单文件上限 4 MiB，超过就轮转。与 daemon 日志上限一致，便于统一预期。 */
    private static final long MAX_BYTES = 4L * 1024 * 1024;

    private OpsLog() {
    }

    /**
     * 追加一条操作记录。op 是动作名（send_start/send_done/probe_result/recv_done/pair_ok/…）。
     * 写入失败只打 warn，绝不影响主流程——日志写不进去不该把已经完成的传输改判成失败。
     */
    static void append(Context ctx, String op, JSONObject extra) {
        try {
            JSONObject o = new JSONObject();
            o.put("ts", System.currentTimeMillis() / 1000);
            o.put("op", op);
            if (extra != null) {
                java.util.Iterator<String> it = extra.keys();
                while (it.hasNext()) {
                    String k = it.next();
                    o.put(k, extra.get(k));
                }
            }
            appendRaw(ctx.getFilesDir(), o.toString());
        } catch (Exception e) {
            Log.w("TONGCHUAN-OPS", "ops-log append failed: " + e.getClass().getSimpleName());
        }
    }

    /** 是否需要轮转：单文件达到上限就改名归档。抽成纯函数便于单测边界。 */
    static boolean shouldRotate(long currentLen) {
        return currentLen >= MAX_BYTES;
    }

    /**
     * 把一行已经序列化好的 JSON 追加进 dir/ops-log.jsonl；超上限先轮转。
     * 与 Context/JSONObject 解耦，纯 File IO，便于 JVM 单测（Android Context 在单测里不可用）。
     */
    static synchronized void appendRaw(File dir, String line) {
        try {
            File f = new File(dir, FILE);
            if (shouldRotate(f.length())) {
                // 轮转：当前文件改名归档，另起一份新的；旧文件保留不删（规范 #11）
                String stamp = new java.text.SimpleDateFormat("yyyyMMdd-HHmmss",
                        java.util.Locale.US).format(new java.util.Date());
                File archived = new File(dir, "ops-log-" + stamp + ".jsonl");
                if (!f.renameTo(archived)) {
                    Log.w("TONGCHUAN-OPS", "ops-log 轮转改名失败，继续追加当前文件");
                }
            }
            try (FileOutputStream out = new FileOutputStream(f, true)) {
                out.write((line + "\n").getBytes(StandardCharsets.UTF_8));
            }
        } catch (Exception e) {
            Log.w("TONGCHUAN-OPS", "ops-log append failed: " + e.getClass().getSimpleName());
        }
    }

    /**
     * 把 ops-log.jsonl 拷到公共下载目录，供 `adb pull` 取回；返回目标绝对路径，失败返回 null。
     * 优先写 /sdcard/Download（本项目已申请所有文件访问）；无权限时退回应用专属外部目录。
     */
    static String dump(Context ctx) {
        File src = new File(ctx.getFilesDir(), FILE);
        if (!src.exists()) {
            // 没有日志就建一个空文件，避免 adb pull 因文件不存在而报错
            try {
                src.createNewFile();
            } catch (Exception ignored) {
            }
        }
        String name = "tongchuan-ops-log.jsonl";
        File dst = new File(Environment.getExternalStoragePublicDirectory(
                Environment.DIRECTORY_DOWNLOADS), name);
        if (copy(src, dst)) {
            return dst.getAbsolutePath();
        }
        // 退回应用专属外部目录（无需额外权限，adb pull 亦可读）
        File ext = ctx.getExternalFilesDir(null);
        if (ext != null) {
            File dst2 = new File(ext, name);
            if (copy(src, dst2)) {
                return dst2.getAbsolutePath();
            }
        }
        return null;
    }

    private static boolean copy(File src, File dst) {
        try (InputStream in = new FileInputStream(src);
             OutputStream out = new FileOutputStream(dst)) {
            byte[] b = new byte[8192];
            int n;
            while ((n = in.read(b)) > 0) {
                out.write(b, 0, n);
            }
            return true;
        } catch (Exception e) {
            Log.w("TONGCHUAN-OPS", "dump copy failed: " + e.getClass().getSimpleName());
            return false;
        }
    }
}
