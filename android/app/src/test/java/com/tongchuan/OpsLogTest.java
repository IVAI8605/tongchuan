package com.tongchuan;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.List;

/**
 * v1.1.0：操作记录写入的纯逻辑回归。
 * 只测与 Android 运行时无关的部分（appendRaw 的 File IO、shouldRotate 的边界）——
 * Context/JSONObject 在单元测试里是 stub，不能在这里用。
 */
public class OpsLogTest {

    @Test
    public void should_rotate_only_at_limit() {
        long max = 4L * 1024 * 1024;
        assertFalse(OpsLog.shouldRotate(max - 1));
        assertTrue(OpsLog.shouldRotate(max));
        assertTrue(OpsLog.shouldRotate(max + 10));
    }

    @Test
    public void append_raw_appends_and_never_overwrites() throws Exception {
        File dir = Files.createTempDirectory("opslog").toFile();
        try {
            OpsLog.appendRaw(dir, "{\"op\":\"send_start\"}");
            OpsLog.appendRaw(dir, "{\"op\":\"send_done\"}");
            File f = new File(dir, "ops-log.jsonl");
            List<String> lines = Files.readAllLines(f.toPath(), StandardCharsets.UTF_8);
            assertEquals("两次追加应得到两行", 2, lines.size());
            assertTrue(lines.get(0).contains("\"send_start\""));
            assertTrue(lines.get(1).contains("\"send_done\""));
        } finally {
            File[] fs = dir.listFiles();
            if (fs != null) {
                for (File x : fs) {
                    x.delete();
                }
            }
            dir.delete();
        }
    }
}
