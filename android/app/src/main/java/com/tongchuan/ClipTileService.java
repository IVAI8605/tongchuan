package com.tongchuan;

import android.app.PendingIntent;
import android.content.Intent;
import android.os.Build;
import android.service.quicksettings.Tile;
import android.service.quicksettings.TileService;

/**
 * v0.8 快捷设置磁贴：点一下等于「把当前剪贴板推到对端」；同步没开时先开同步。
 *
 * 为什么必须拉起界面：Android 10 起后台读剪贴板会被系统拦成空内容，只有前台应用能读。
 * 所以磁贴不能自己读，只能把 MainActivity 带到前台，由它读内容再调 JNI 推送——
 * 这就是功能展望 §3.2 里说的「保底一键」，不是偷懒。
 */
public class ClipTileService extends TileService {

    @Override
    public void onStartListening() {
        super.onStartListening();
        Tile t = getQsTile();
        if (t != null) {
            t.setState(ClipService.isRunning() ? Tile.STATE_ACTIVE : Tile.STATE_INACTIVE);
            t.setLabel(ClipService.isRunning() ? "推送剪贴板" : "剪贴板同步");
            t.updateTile();
        }
    }

    @Override
    public void onClick() {
        super.onClick();
        if (!ClipService.isRunning()) {
            ClipService.start(this);
            Tile t = getQsTile();
            if (t != null) {
                t.setState(Tile.STATE_ACTIVE);
                t.setLabel("推送剪贴板");
                t.updateTile();
            }
            return;
        }
        Intent i = new Intent(this, MainActivity.class)
                .putExtra("clip_push", true)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            // Android 14 起必须用 PendingIntent 版本，旧的 Intent 重载已废弃
            startActivityAndCollapse(PendingIntent.getActivity(
                    this, 0, i, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT));
        } else {
            startActivityAndCollapse(i);
        }
    }
}
