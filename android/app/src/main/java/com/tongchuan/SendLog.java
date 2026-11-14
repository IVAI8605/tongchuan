package com.tongchuan;

import android.content.Context;

import org.json.JSONObject;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.List;

/**
 * v0.7：手机侧发送历史（追加写的 JSONL）。
 *
 * 为什么单独存一份：电脑端有 `tc history` 可查，手机发出去的那几批以前结果只留在界面上，
 * 关掉应用就没了，事后想确认「昨天那张图发到哪台机器了」只能靠回忆。
 * 只记次数、字节、目标名与失败原因；**不记录文件路径与文件名**，避免把用户内容写进日志。
 */
final class SendLog {

    private static final String FILE = "send-history.jsonl";

    private SendLog() {
    }

    /** 一条发送记录。字段与电脑端 `tc history` 同名同义，便于两边对照。 */
    static final class Record {
        long ts;
        String name = "";
        String addr = "";
        long files;
        long bytes;
        boolean ok;
        String reason = "";
        /** v0.11：接收端回报的落点文件夹（可能多个，逗号分隔），失败时为空 */
        String dest = "";
        /** v0.12：按「同路径同内容」被接收端跳过的文件数（强制重传时为 0） */
        long skipped;
    }

    /** 追加一条；失败不抛给调用方——历史写不进去不该把已经发完的结果改判成失败。 */
    static void append(Context ctx, Record r) {
        try {
            JSONObject o = new JSONObject();
            o.put("ts", r.ts);
            o.put("name", r.name);
            o.put("addr", r.addr);
            o.put("files", r.files);
            o.put("bytes", r.bytes);
            o.put("ok", r.ok);
            o.put("reason", r.reason);
            o.put("dest", r.dest);
            o.put("skipped", r.skipped);
            synchronized (SendLog.class) {
                try (FileOutputStream out = new FileOutputStream(new File(ctx.getFilesDir(), FILE), true)) {
                    out.write((o.toString() + "\n").getBytes(StandardCharsets.UTF_8));
                }
            }
        } catch (Exception e) {
            android.util.Log.w("TONGCHUAN-SEND", "history append failed: " + e.getClass().getSimpleName());
        }
    }

    /** 读最近 limit 条，最新的在前。坏行跳过：升级前后字段可能不一致，不能因为一行坏掉整表消失。 */
    static List<Record> recent(Context ctx, int limit) {
        List<Record> all = new ArrayList<>();
        File f = new File(ctx.getFilesDir(), FILE);
        if (!f.exists()) {
            return all;
        }
        try (BufferedReader r = new BufferedReader(
                new InputStreamReader(new FileInputStream(f), StandardCharsets.UTF_8))) {
            String line;
            while ((line = r.readLine()) != null) {
                try {
                    JSONObject o = new JSONObject(line);
                    Record rec = new Record();
                    rec.ts = o.optLong("ts");
                    rec.name = o.optString("name", "");
                    rec.addr = o.optString("addr", "");
                    rec.files = o.optLong("files");
                    rec.bytes = o.optLong("bytes");
                    rec.ok = o.optBoolean("ok", false);
                    rec.reason = o.optString("reason", "");
                    rec.dest = o.optString("dest", "");
                    rec.skipped = o.optLong("skipped");
                    all.add(rec);
                } catch (Exception ignored) {
                    // 单行坏掉就跳过，不影响其余记录
                }
            }
        } catch (Exception e) {
            android.util.Log.w("TONGCHUAN-SEND", "history read failed: " + e.getClass().getSimpleName());
        }
        List<Record> out = new ArrayList<>();
        for (int i = all.size() - 1; i >= 0 && out.size() < limit; i--) {
            out.add(all.get(i));
        }
        return out;
    }
}
