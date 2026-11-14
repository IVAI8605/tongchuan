package com.tongchuan;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * 开机后把常驻接收拉起来。只在用户主动开过常驻后才有意义，
 * 所以用 SharedPreferences 记一个开关，避免装了没开就被开机自启。
 */
public class BootReceiver extends BroadcastReceiver {

    static final String PREFS = "tongchuan";
    static final String KEY_WANT = "want_running";

    @Override
    public void onReceive(Context ctx, Intent intent) {
        String action = intent == null ? null : intent.getAction();
        if (!Intent.ACTION_BOOT_COMPLETED.equals(action)
                && !Intent.ACTION_MY_PACKAGE_REPLACED.equals(action)) {
            return;
        }
        boolean want = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .getBoolean(KEY_WANT, false);
        if (want) {
            ReceiverService.start(ctx);
        }
    }
}
