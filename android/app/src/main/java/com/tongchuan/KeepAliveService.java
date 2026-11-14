package com.tongchuan;

import android.accessibilityservice.AccessibilityService;
import android.content.Context;
import android.provider.Settings;
import android.text.TextUtils;
import android.view.accessibility.AccessibilityEvent;

/**
 * 保活托底：不拦、不改、不读任何界面，只是让系统认为本应用有一个常驻的前台型服务，
 * 从而降低被系统清后台带走的概率。所有回调都立即返回，不做任何计算，尽量省电。
 * 用户可以在系统设置里随时关掉，不关也能用，只是后台存活时间会短一些。
 */
public class KeepAliveService extends AccessibilityService {

    @Override
    public void onServiceConnected() {
        super.onServiceConnected();
        // 不申请任何额外能力；连接上本身就是目的
    }

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {
        // 刻意留空：拿到事件什么都不做，避免任何无谓的 CPU 唤醒
    }

    @Override
    public void onInterrupt() {
        // 系统主动打断时不处理，服务会自动重连
    }

    /** 给界面判断用：无障碍里是否已经勾选本服务。 */
    public static boolean isEnabled(Context ctx) {
        String expected = ctx.getPackageName() + "/" + KeepAliveService.class.getName();
        String enabled = Settings.Secure.getString(
                ctx.getContentResolver(), Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES);
        if (TextUtils.isEmpty(enabled)) {
            return false;
        }
        for (String s : enabled.split(":")) {
            if (expected.equalsIgnoreCase(s)) {
                return true;
            }
        }
        return false;
    }
}
