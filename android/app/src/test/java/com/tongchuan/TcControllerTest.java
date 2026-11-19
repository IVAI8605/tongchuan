package com.tongchuan;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/**
 * 纯函数回归：这些文案与单位换算直接决定界面显示，改坏了用户能直接看到。
 * 只测静态纯函数，不需要 Android 运行时。
 */
public class TcControllerTest {

    @Test
    public void human_bytes() {
        assertEquals("512 B", TcController.human(512));
    }

    @Test
    public void human_kb() {
        assertTrue(TcController.human(1024), TcController.human(1024).endsWith("KB"));
    }

    @Test
    public void human_mb() {
        assertTrue(TcController.human(1024 * 1024).endsWith("MB"));
    }

    @Test
    public void human_gb() {
        assertTrue(TcController.human(1024L * 1024 * 1024).endsWith("GB"));
    }

    @Test
    public void friendly_untrusted_shows_short_fp() {
        String s = TcController.friendlyFailure("untrusted peer", "ABCDEF1234567890");
        assertTrue(s, s.contains("对方还没有信任"));
        assertTrue(s, s.contains("ABCDEF12…"));
    }

    @Test
    public void friendly_connection_reset() {
        assertTrue(TcController.friendlyFailure("connection reset", "").contains("连接中断"));
    }

    @Test
    public void friendly_pair_code() {
        assertTrue(TcController.friendlyFailure("bad pair code", "").contains("配对码不对"));
    }

    @Test
    public void friendly_unknown() {
        assertEquals("发送失败：未知原因", TcController.friendlyFailure("", ""));
    }
}
