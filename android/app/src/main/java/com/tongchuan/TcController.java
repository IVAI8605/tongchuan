package com.tongchuan;

import android.Manifest;
import android.app.Activity;
import android.app.AlertDialog;
import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.net.Uri;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.text.InputType;
import android.view.Gravity;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.TextView;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.text.SimpleDateFormat;
import java.util.ArrayList;
import java.util.Date;
import java.util.EnumMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * 2026-11-19 UI 重构：原 MainActivity 的业务逻辑整体搬到这里，View 构建全部移除。
 *
 * UI 状态写在 {@link UiState}（Compose 快照状态）里，由 Compose 侧读取渲染；
 * 服务、JNI、权限、发现/信任/配对/发送/剪贴板的行为与原 MainActivity 完全一致，
 * 只把「界面出口」从 TextView/Button 换成了状态 setter。
 *
 * 配对二维码对话框暂时仍用 AlertDialog（带自定义视图与倒计时），
 * 在设备页任务里再换成 Compose 对话框。
 */
public class TcController {

    private static final int REQ_TREE = 100;
    private static final int REQ_SEND_FILES = 101;
    private static final int REQ_SEND_TREE = 102;
    /** v0.20 扫码配对：从 ScanActivity 拿回二维码文本 */
    private static final int REQ_SCAN = 103;

    private final Activity activity;
    private final UiState ui = new UiState();
    private final Handler uiTick = new Handler(Looper.getMainLooper());
    /** v1.1.0：接收页实时进度，接收期间每 1 秒刷一次；只在前台跑，进后台就停，省电。 */
    private final Runnable recvTick = new Runnable() {
        @Override
        public void run() {
            refreshRecvProgress();
            uiTick.postDelayed(this, 1000);
        }
    };

    /** 启停是异步的：这段时间内不要用真实状态覆盖「正在启动/停止…」 */
    private volatile boolean recvPending;
    /** v1.4.1：上一轮是否正在接收；用于在「接收结束」这一拍刷新「最近接收」列表 */
    private volatile boolean wasReceiving;
    /**
     * v1.4.1：上一轮看到的 last_recv 内容。小文件几秒钟就收完，1 秒轮询很可能一次都没看到
     * active=true，只靠「在收 → 收完」的跳变会漏刷；这里直接比对接收记录，任何一次新接收都刷新。
     */
    private volatile String seenLastRecv;
    private volatile boolean sendPaused;
    /** 用户点了取消：当前会话会被中断，后面排队的其它目标也不再开始 */
    private volatile boolean sendAborted;

    /** 已选中的绝对路径；一次发送里既可能是目录也可能是文件 */
    private final List<String> pickedPaths = new ArrayList<>();
    private String targetAddr = "";
    private String targetFp = "";
    private String targetName = "";
    /** 本机指纹：被对方拒绝时用户要拿它去批准 */
    private String selfFp = "";
    /** v0.7：已选目标（addr / fp / name），支持一次选多台顺序发 */
    private final List<String[]> selectedPeers = new ArrayList<>();
    /** v0.16：对端可用落点（id / name） */
    private final List<String[]> destOptions = new ArrayList<>();
    private String destId = "";
    /** 界面当前显示的接收目录；从「所有文件访问」设置页回来时靠它判断落点是否已自动切换 */
    private String shownDest;
    /** 最近一次发现结果：勾选/取消后据此重画列表，不必再发一轮广播 */
    private String lastPeersJson;

    public TcController(Activity activity) {
        this.activity = activity;
    }

    public UiState getUi() {
        return ui;
    }

    // ---- 生命周期 ----

    public void onCreate(Intent intent) {
        ui.setVersionName(versionName());
        // v1.2.0：安全总开关默认关，读持久化值
        ui.setSecurityEnabled(prefBool("security_enabled", false));
        // v0.12：打开应用就自动常驻（含后台保活），用户不必再点「启动常驻接收」
        autoKeepAlive();
        // 首次进入也要读一次信任库：只靠 onResume 的话，界面刚建好时列表是空的
        refreshTrust();
        // 记住上次选过的发送目标，避免每次都要重新发现
        restoreSendTarget();
        ui.setReceiveDirText(receiveDirText());
        shownDest = Dest.current(activity).getAbsolutePath();
        // 从分享菜单进来的要先把文件收下，再让用户选目标
        handleShareIntent(intent);
        maybeRunProbe(intent);
    }

    /** 应用已在后台时被再次分享：复用当前界面，不叠新的 Activity（manifest 里 singleTop）。 */
    public void onNewIntent(Intent intent) {
        handleShareIntent(intent);
    }

    public void onResume() {
        // 从「所有文件访问」设置页回来时，默认落点会自动从私有目录切到 Download/局域网通传。
        // 不在这里比对的话，界面还显示旧路径、接收端还往旧目录写，用户看到的就是「授权没生效」。
        String now = Dest.current(activity).getAbsolutePath();
        if (shownDest != null && !shownDest.equals(now)) {
            shownDest = now;
            if (ReceiverService.isRunning()) {
                ReceiverService.restart(activity);
            }
            activity.recreate();
            return;
        }
        refreshAll();
        uiTick.removeCallbacks(recvTick);
        uiTick.post(recvTick);
    }

    /** 一次性把各页状态都刷到最新（onResume 与「刷新状态」共用）。 */
    public void refreshAll() {
        refresh();
        refreshTrust();
        refreshRecent();
        refreshSendHistory();
    }

    public void onPause() {
        // 界面不在前台就别再刷了，省电也避免持有 Activity 引用        uiTick.removeCallbacks(recvTick);
    }

    /**
     * 选完目录后把 SAF 树 URI 还原成真实路径存下来。
     * 选到网盘/U 盘这类没有稳定真实路径的位置时明确拒绝，而不是给个能选却写不进去的假选项。
     */
    public void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (resultCode != Activity.RESULT_OK || data == null) {
            return;
        }
        // ---- 发送：选文件（可多选）----
        if (requestCode == REQ_SEND_FILES) {
            pickedPaths.clear();
            List<Uri> uris = new ArrayList<>();
            ClipData clip = data.getClipData();
            if (clip != null) {
                for (int i = 0; i < clip.getItemCount(); i++) {
                    uris.add(clip.getItemAt(i).getUri());
                }
            } else if (data.getData() != null) {
                uris.add(data.getData());
            }
            resolvePickedAsync(uris);
            return;
        }
        // ---- 发送：选整个文件夹 ----
        if (requestCode == REQ_SEND_TREE) {
            String p = data.getData() == null ? null : Dest.resolveTreePath(data.getData());
            if (p == null) {
                toast("只支持手机内部存储里的文件夹；网盘/U 盘暂时发不了");
                return;
            }
            pickedPaths.clear();
            pickedPaths.add(p);
            updatePickedLabel(0);
            return;
        }
        // ---- 发送：扫二维码配对（v0.20）----
        if (requestCode == REQ_SCAN) {
            handleScanResult(data.getStringExtra(ScanActivity.EXTRA_RESULT));
            return;
        }
        if (requestCode != REQ_TREE || data.getData() == null) {
            return;
        }
        String path = Dest.resolveTreePath(data.getData());
        if (path == null) {
            toast("这个位置不是手机内置存储，暂时不支持；请选「内部存储」里的文件夹");
            return;
        }
        Dest.save(activity, path);
        toast("接收目录已改为：" + path);
        // 落点变了要换配置重开接收端，否则旧配置里的 destination 还指在旧目录
        if (ReceiverService.isRunning()) {
            ReceiverService.restart(activity);
        }
        activity.recreate();
    }

    // ---- 分享入口 ----

    /**
     * v0.7 分享菜单入口：把系统分享过来的 URI 转成真实路径放进「已选内容」。
     * 只认能定位到手机存储的项；网盘/受限 provider 转不出来就跳过并计数，不假装选上了。
     */
    private void handleShareIntent(Intent intent) {
        if (intent == null) {
            return;
        }
        String action = intent.getAction();
        if (!Intent.ACTION_SEND.equals(action) && !Intent.ACTION_SEND_MULTIPLE.equals(action)) {
            return;
        }
        // 处理完抹掉 action：界面重建（旋屏、改接收目录）时不能再加一遍
        intent.setAction(null);
        List<Uri> uris = new ArrayList<>();
        ClipData clip = intent.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) {
                uris.add(clip.getItemAt(i).getUri());
            }
        }
        if (uris.isEmpty()) {
            Uri single = intent.getParcelableExtra(Intent.EXTRA_STREAM);
            if (single != null) {
                uris.add(single);
            }
        }
        if (uris.isEmpty()) {
            return;
        }
        pickedPaths.clear();
        // 与「选择文件」同一条解析路径：多选时也在后台线程做，避免分享多张图卡住界面
        resolvePickedAsync(uris);
        // 顺手列一次附近设备：分享过来的用户下一步必然要选目标
        discoverPeersAsync();
    }

    // ---- 自动化探针（保留，行为不变）----

    /**
     * M1 联调用的一次性入口：由 adb 触发，结果只写 logcat（tag=TONGCHUAN-PROBE）。
     * 例：am start -n com.tongchuan/.MainActivity --es tc_cmd list --es tc_path /storage/emulated/0
     * 它不参与任何业务逻辑，M3 做正式发送界面时整段删除。
     */
    private void maybeRunProbe(Intent intent) {
        if (intent == null) {
            return;
        }
        final String cmd = intent.getStringExtra("tc_cmd");
        if (cmd == null) {
            return;
        }
        final String path = intent.getStringExtra("tc_path");
        final String target = intent.getStringExtra("tc_target");
        final String fp = intent.getStringExtra("tc_fp");
        final String paths = intent.getStringExtra("tc_paths");
        final String dest = intent.getStringExtra("tc_dest");
        final long timeoutMs = intent.getLongExtra("tc_timeout", 6000);
        final String identityDir = ReceiverService.identityDir(activity).getAbsolutePath();
        new Thread(() -> {
            String out;
            if ("list".equals(cmd)) {
                out = TcNative.listPath(path == null ? "/storage/emulated/0" : path);
            } else if ("discover".equals(cmd)) {
                out = runDiscover(timeoutMs);
            } else if ("progress".equals(cmd)) {
                out = TcNative.sendProgress();
            } else if ("dests".equals(cmd)) {
                out = TcNative.listDestinations(target, fp, "", identityDir, Build.MODEL);
            } else if ("send".equals(cmd)) {
                String pj = paths;
                if (pj == null && path != null) {
                    pj = new JSONArray().put(path).toString();
                }
                Thread mon = new Thread(() -> {
                    long deadline = System.currentTimeMillis() + 3000;
                    boolean started = false;
                    // v1.1.0：只在进度签名（阶段+百分比）变化时才打日志。
                    // 原来每 250ms 无条件打一条，2 分钟刷 500+ 条把 logcat 缓冲冲爆、反而冲掉有用信息。
                    String lastSig = "";
                    while (true) {
                        String p = TcNative.sendProgress();
                        boolean active = p != null && p.contains("\"active\":true");
                        if (active) {
                            started = true;
                            String sig = p;
                            try {
                                JSONObject o = new JSONObject(p);
                                sig = o.optString("phase", "") + ":" + o.optInt("percent", 0);
                            } catch (Exception ignored) {
                            }
                            if (!sig.equals(lastSig)) {
                                android.util.Log.i("TONGCHUAN-PROBE", "PROGRESS " + p);
                                lastSig = sig;
                            }
                        } else if (started) {
                            android.util.Log.i("TONGCHUAN-PROBE", "PROGRESS-DONE " + p);
                            return;
                        } else if (System.currentTimeMillis() > deadline) {
                            return;
                        }
                        try {
                            Thread.sleep(1000);
                        } catch (InterruptedException e) {
                            return;
                        }
                    }
                }, "tc-progress");
                mon.start();
                SendService.start(activity, target, fp, pj, "", identityDir, Build.MODEL,
                        intent.getBooleanExtra("tc_force", false), dest == null ? "" : dest);
                long t0 = System.currentTimeMillis();
                String r = null;
                while (r == null) {
                    r = SendService.takeResult();
                    if (r == null && !SendService.isRunning()
                            && System.currentTimeMillis() - t0 > 3000) {
                        r = "{\"ok\":false,\"reason\":\"发送服务未能启动或被系统终止\"}";
                    }
                    if (r == null) {
                        try {
                            Thread.sleep(200);
                        } catch (InterruptedException e) {
                            break;
                        }
                    }
                }
                out = r;
                try {
                    JSONObject o = new JSONObject(out == null ? "{}" : out);
                    if (!o.optBoolean("ok")) {
                        android.util.Log.i("TONGCHUAN-PROBE",
                                "FRIENDLY " + friendlyFailure(o.optString("reason", "")));
                    }
                } catch (Exception ignored) {
                }
            } else if ("dump-log".equals(cmd)) {
                // v1.1.0：把 ops-log.jsonl 拷到公共目录，供 adb pull 取回
                String p = OpsLog.dump(activity);
                out = p != null
                        ? "{\"ok\":true,\"path\":\"" + p.replace("\\", "\\\\") + "\"}"
                        : "{\"ok\":false,\"reason\":\"ops-log 导出失败（可能无写权限）\"}";
            } else {
                out = "{\"ok\":false,\"reason\":\"unknown tc_cmd\"}";
            }
            // v1.1.0：每条探针命令执行完都记一条 probe_result，事后能区分「哪些操作是探针触发的」
            try {
                JSONObject e = new JSONObject();
                e.put("cmd", cmd);
                e.put("ok", out != null && out.contains("\"ok\":true"));
                OpsLog.append(activity, "probe_result", e);
            } catch (Exception ignored) {
            }
            android.util.Log.i("TONGCHUAN-PROBE", out == null ? "null" : out);
        }, "tc-probe").start();
    }

    /**
     * 收组播要持 MulticastLock，否则部分 ROM 会直接丢组播包。
     * 只在一轮发现期间持有，不长期占用（长期持锁会额外耗电）。
     */
    private String runDiscover(long timeoutMs) {
        android.net.wifi.WifiManager wm = (android.net.wifi.WifiManager)
                activity.getApplicationContext().getSystemService(Context.WIFI_SERVICE);
        android.net.wifi.WifiManager.MulticastLock lock =
                wm.createMulticastLock("tongchuan:discover");
        lock.setReferenceCounted(false);
        lock.acquire();
        try {
            return TcNative.discoverPeers(timeoutMs,
                    ReceiverService.identityDir(activity).getAbsolutePath());
        } finally {
            lock.release();
        }
    }

    /**
     * 接收目录说明。
     * 新默认是「内置存储 / Download / 局域网通传」；没给「所有文件访问」时会退回应用私有目录，
     * 这里必须写清实际落点，否则用户会觉得显示的路径和预期对不上。
     */
    private String receiveDirText() {
        File cur = Dest.current(activity);
        StringBuilder sb = new StringBuilder();
        sb.append("接收目录：\n").append(Dest.displayPath(cur))
                .append("\n（").append(cur.getAbsolutePath()).append("）");
        if (!Dest.hasAllFilesAccess()) {
            sb.append("\n未授予「所有文件访问」，暂存应用私有目录；")
                    .append("点上方「授予所有文件权限」后会自动切到 Download/")
                    .append(Dest.DEFAULT_DIR_NAME).append("。");
        }
        File old = Dest.legacyDir(activity);
        if (!old.getAbsolutePath().equals(cur.getAbsolutePath()) && hasEntries(old)) {
            sb.append("\n旧默认目录里已有文件（未迁移、未删除）：\n").append(old.getAbsolutePath());
        }
        sb.append("\n\n同一个 Wi-Fi 下，电脑端即可向本机投递。")
                .append("\n\n增强存活只需在无障碍里勾选「通传」，它不读屏幕、不代替你操作，随时可关。");
        return sb.toString();
    }

    /** 目录里有没有内容（旧默认目录由本应用直接写入，只看一层就够）。 */
    private static boolean hasEntries(File dir) {
        String[] names = dir.list();
        return names != null && names.length > 0;
    }

    // ---- 设备页动作 ----

    public void startRecv() {
        ensureNotificationPermission();
        setWantRunning(true);
        recvPending = true;
        ui.setRecvPending(true);
        ui.setStatusText("常驻：正在启动…（等待端口就绪）");
        ui.setStatusOk(false);
        ReceiverService.start(activity);
        waitRecvState(true);
    }

    public void stopRecv() {
        setWantRunning(false);
        recvPending = true;
        ui.setRecvPending(true);
        ui.setStatusText("常驻：正在停止…");
        ui.setStatusOk(false);
        activity.stopService(new Intent(activity, ReceiverService.class));
        waitRecvState(false);
    }

    public void openReceiveDirPicker() {
        try {
            activity.startActivityForResult(
                    new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE), REQ_TREE);
        } catch (Exception e) {
            toast("打不开目录选择器：" + e.getMessage());
        }
    }

    public void restoreDefaultRecvDir() {
        Dest.clear(activity);
        if (ReceiverService.isRunning()) {
            ReceiverService.restart(activity);
        }
        toast("已恢复默认：" + Dest.displayPath(Dest.current(activity)));
        activity.recreate();
    }

    public void requestAllFiles() {
        if (Dest.hasAllFilesAccess()) {
            return;
        }
        try {
            Intent i = new Intent(
                    android.provider.Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION);
            i.setData(Uri.parse("package:" + activity.getPackageName()));
            activity.startActivity(i);
        } catch (Exception e) {
            toast("打不开权限页：" + e.getMessage());
        }
    }

    public void openAccessibility() {
        try {
            activity.startActivity(
                    new Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS));
        } catch (Exception e) {
            toast("打不开无障碍设置：" + e.getMessage());
        }
    }

    public void openBatteryWhiteList() {
        try {
            Intent i = new Intent(
                    android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS);
            i.setData(Uri.parse("package:" + activity.getPackageName()));
            activity.startActivity(i);
        } catch (Exception e) {
            try {
                activity.startActivity(new Intent(
                        android.provider.Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS));
            } catch (Exception ignored) {
                // 个别 ROM 没有这个页面：不影响接收
            }
        }
    }

    // ---- 发送：目标与内容 ----

    /**
     * v0.12：多选视频时界面会卡住一会儿才回到待发送页——原因是每一项都要查 provider、
     * 反查 fd，最坏还要把内容整份复制到缓存，这些之前全在主线程跑。
     * 现在先给一句「正在读取」，解析放到后台线程，完成后一次性刷新摘要。
     */
    private void resolvePickedAsync(List<Uri> uris) {
        if (uris == null || uris.isEmpty()) {
            updatePickedLabel(0);
            return;
        }
        ui.setPickedText("正在读取已选的 " + uris.size() + " 项…");
        final List<Uri> snapshot = new ArrayList<>(uris);
        new Thread(() -> {
            List<String> resolved = new ArrayList<>();
            int skipped = 0;
            for (Uri u : snapshot) {
                String p = Dest.resolveAnyPath(activity, u);
                if (p == null) {
                    skipped++;
                } else {
                    resolved.add(p);
                }
            }
            final int skip = skipped;
            activity.runOnUiThread(() -> {
                pickedPaths.clear();
                pickedPaths.addAll(resolved);
                // 日志只记数量，不记任何路径/文件名
                android.util.Log.i("TONGCHUAN-SEND",
                        "picked=" + pickedPaths.size() + " skipped=" + skip);
                updatePickedLabel(skip);
            });
        }, "tc-pick").start();
    }

    /**
     * 待发送摘要：v0.12 起不列文件名，只给「哪个文件夹、多少个文件」。
     * 文件名一多会把界面撑满，而且用户刚选完就知道选了什么，不需要在这里复核。
     */
    private void updatePickedLabel(int skipped) {
        if (pickedPaths.isEmpty()) {
            ui.setPickedItems(java.util.Collections.emptyList());
            ui.setPickedText(skipped > 0
                    ? "选中的 " + skipped + " 项都无法定位到手机存储，请改用「选择文件夹」或从「内部存储」里选"
                    : "尚未选择要发送的内容");
            return;
        }
        StringBuilder sb = new StringBuilder();
        File only = new File(pickedPaths.get(0));
        if (pickedPaths.size() == 1 && only.isDirectory()) {
            sb.append("已选择「").append(only.getName()).append("」文件夹");
        } else {
            java.util.LinkedHashMap<String, Integer> groups = new java.util.LinkedHashMap<>();
            for (String p : pickedPaths) {
                File parent = new File(p).getParentFile();
                String name = parent == null ? "手机存储" : parent.getName();
                Integer old = groups.get(name);
                groups.put(name, old == null ? 1 : old + 1);
            }
            int count = pickedPaths.size();
            String first = groups.keySet().iterator().next();
            if (groups.size() == 1) {
                sb.append("已选择「").append(first).append("」文件夹的 ").append(count).append(" 个文件");
            } else {
                sb.append("已选择「").append(first).append("」等 ").append(groups.size())
                        .append(" 个文件夹的 ").append(count).append(" 个文件");
            }
        }
        if (skipped > 0) {
            sb.append("（另有 ").append(skipped).append(" 项无法定位，已跳过）");
        }
        // v1.2.1：缩略展示已选内容（最多 8 条名字，超出给总数）
        java.util.List<String> items = new ArrayList<>();
        for (String p : pickedPaths) {
            items.add(new File(p).getName());
            if (items.size() >= 8) {
                items.add("…共 " + pickedPaths.size() + " 项");
                break;
            }
        }
        ui.setPickedItems(items);
        ui.setPickedText(sb.toString());
    }

    private void updateTargetLabel() {
        refreshDestinations();
        if (selectedPeers.isEmpty()) {
            ui.setSendTargetText("尚未选择目标");
            return;
        }
        String[] first = selectedPeers.get(0);
        String firstName = first[2] == null || first[2].isEmpty() ? "未命名设备" : first[2];
        if (selectedPeers.size() == 1) {
            ui.setSendTargetText(firstName + "\n" + first[0]);
            return;
        }
        StringBuilder names = new StringBuilder();
        for (int i = 0; i < selectedPeers.size(); i++) {
            String[] t = selectedPeers.get(i);
            if (i > 0) {
                names.append("、");
            }
            names.append(t[2] == null || t[2].isEmpty() ? t[0] : t[2]);
        }
        ui.setSendTargetText("已选 " + selectedPeers.size() + " 个目标\n" + names);
    }

    /**
     * v0.16：选中单个目标后查一次可用落点。查询失败不影响发送——退回默认落点，
     * 用户仍能照常发；多目标时不提供选择，各用对方的默认位置。
     */
    private void refreshDestinations() {
        destOptions.clear();
        destId = "";
        ui.setDestId("");
        ui.setDestOptions(new ArrayList<DestOption>());
        if (selectedPeers.size() != 1) {
            ui.setDestText(selectedPeers.isEmpty() ? "默认" : "各自默认（多目标）");
            return;
        }
        final String[] t = selectedPeers.get(0);
        final String pair = ui.getPairCode();
        final String identityDir = ReceiverService.identityDir(activity).getAbsolutePath();
        ui.setDestText("查询中…");
        new Thread(() -> {
            String out = TcNative.listDestinations(t[0], t[1], pair, identityDir, Build.MODEL);
            activity.runOnUiThread(() -> applyDestinations(t[0], out));
        }, "tc-dests").start();
    }

    private void applyDestinations(String addr, String out) {
        // 查询期间目标被换掉：结果作废，避免把 A 的落点用到 B 上
        if (selectedPeers.size() != 1 || !selectedPeers.get(0)[0].equals(addr)) {
            return;
        }
        destOptions.clear();
        try {
            JSONObject o = new JSONObject(out == null ? "{}" : out);
            JSONArray arr = o.optJSONArray("destinations");
            if (o.optBoolean("ok") && arr != null) {
                for (int i = 0; i < arr.length(); i++) {
                    JSONObject d = arr.getJSONObject(i);
                    String id = d.optString("id");
                    if (id.isEmpty()) {
                        continue;
                    }
                    destOptions.add(new String[]{id, d.optString("name", id),
                            d.optBoolean("is_default", false) ? "1" : "0"});
                }
            }
        } catch (Exception ignored) {
        }
        List<DestOption> opts = new ArrayList<>();
        for (String[] d : destOptions) {
            opts.add(new DestOption(d[0], d[1], "1".equals(d[2])));
        }
        ui.setDestOptions(opts);
        // v1.2.1：显示默认落点名 +（默认文件夹）；不暴露真实路径
        String dn = defaultDestName();
        ui.setDestText(dn.isEmpty() ? "默认文件夹" : dn + "（默认文件夹）");
    }

    /** v1.2.1：当前对端默认落点的显示名（来自 is_default 项）；没有返回空串。 */
    private String defaultDestName() {
        for (String[] d : destOptions) {
            if (d.length > 2 && "1".equals(d[2])) {
                return d[1];
            }
        }
        return "";
    }

    /** 点落点行：单目标且有可选时弹出选择；多目标与无可选都按原口径提示/重查。 */
    public void chooseDestination() {
        if (selectedPeers.size() != 1) {
            toast("多目标时各用对方的默认位置");
            return;
        }
        if (destOptions.isEmpty()) {
            refreshDestinations();
            return;
        }
        ui.setDestDialogVisible(true);
    }

    public void dismissDestDialog() {
        ui.setDestDialogVisible(false);
    }

    /** Compose 对话框回调：which=0 表示默认位置。 */
    public void applyDestination(int which) {
        ui.setDestDialogVisible(false);
        if (which == 0) {
            destId = "";
            ui.setDestId("");
            String dn = defaultDestName();
            ui.setDestText(dn.isEmpty() ? "默认文件夹" : dn + "（默认文件夹）");
        } else {
            String[] d = destOptions.get(which - 1);
            destId = d[0];
            ui.setDestId(destId);
            ui.setDestText("1".equals(d[2]) ? d[1] + "（默认文件夹）" : d[1]);
        }
    }

    private boolean isPeerSelected(String addr) {
        for (String[] t : selectedPeers) {
            if (t[0].equals(addr)) {
                return true;
            }
        }
        return false;
    }

    /** 点一下选中、再点取消；多台可同时选，发送时按选择顺序逐个发。 */
    public void togglePeer(String addr, String fp, String name) {
        for (int i = 0; i < selectedPeers.size(); i++) {
            if (selectedPeers.get(i)[0].equals(addr)) {
                selectedPeers.remove(i);
                updateTargetLabel();
                renderPeers(lastPeersJson);
                return;
            }
        }
        selectedPeers.add(new String[]{addr, fp, name});
        // 记住最后一次选中的那台，下次进应用不用重新发现（多选不落盘，避免过期的目标组合）
        activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE).edit()
                .putString("send_addr", addr)
                .putString("send_fp", fp)
                .putString("send_name", name)
                .apply();
        updateTargetLabel();
        renderPeers(lastPeersJson);
    }

    private static String shortFp(String fp) {
        return fp == null || fp.length() < 8 ? "—" : fp.substring(0, 8);
    }

    private void restoreSendTarget() {
        android.content.SharedPreferences p =
                activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE);
        targetAddr = p.getString("send_addr", "");
        targetFp = p.getString("send_fp", "");
        targetName = p.getString("send_name", "");
        if (!targetAddr.isEmpty() && !targetFp.isEmpty()) {
            selectedPeers.add(new String[]{targetAddr, targetFp, targetName});
        }
        updateTargetLabel();
    }

    public void discoverPeersAsync() {
        ui.setPeers(new ArrayList<PeerEntry>());
        ui.setPeers(listOfDiscovering());
        new Thread(() -> {
            String json = runDiscover(5000);
            activity.runOnUiThread(() -> renderPeers(json));
        }, "tc-discover").start();
    }

    /** 发现中的占位行：语义与原来的「正在发现…」一致。 */
    private List<PeerEntry> listOfDiscovering() {
        List<PeerEntry> list = new ArrayList<>();
        list.add(new PeerEntry("", "", "正在发现…（约 5 秒）", false));
        return list;
    }

    private void renderPeers(String json) {
        lastPeersJson = json;
        List<PeerEntry> list = new ArrayList<>();
        try {
            JSONObject o = new JSONObject(json == null ? "{}" : json);
            JSONArray arr = o.optJSONArray("peers");
            if (arr == null || arr.length() == 0) {
                list.add(new PeerEntry("", "", "没有发现设备：确认在同一 Wi-Fi，且对端已开启接收", false));
                ui.setPeers(list);
                return;
            }
            // 已选目标里那些这次没出现的（下线/换了地址）直接去掉
            List<String> online = new ArrayList<>();
            for (int i = 0; i < arr.length(); i++) {
                online.add(arr.getJSONObject(i).optString("addr"));
            }
            for (int i = selectedPeers.size() - 1; i >= 0; i--) {
                if (!online.contains(selectedPeers.get(i)[0])) {
                    selectedPeers.remove(i);
                }
            }
            updateTargetLabel();
            for (int i = 0; i < arr.length(); i++) {
                JSONObject p = arr.getJSONObject(i);
                String addr = p.optString("addr");
                String fp = p.optString("fingerprint");
                String name = p.optString("name");
                String label = (name.isEmpty() ? "未命名" : name) + "  ·  " + addr
                        + "  ·  " + shortFp(fp);
                list.add(new PeerEntry(addr, fp, label, isPeerSelected(addr)));
            }
            ui.setPeers(list);
        } catch (Exception e) {
            list.add(new PeerEntry("", "", "发现结果解析失败：" + e.getMessage(), false));
            ui.setPeers(list);
        }
    }

    /** 强制重传开关：随清单一起告诉接收端「不要跳过已有文件」。 */
    public void setForce(boolean value) {
        ui.setForce(value);
    }

    public void pickSendFiles() {
        try {
            Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            i.addCategory(Intent.CATEGORY_OPENABLE);
            i.setType("*/*");
            i.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            activity.startActivityForResult(i, REQ_SEND_FILES);
        } catch (Exception e) {
            toast("打不开文件选择器：" + e.getMessage());
        }
    }

    public void pickSendFolder() {
        try {
            activity.startActivityForResult(
                    new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE), REQ_SEND_TREE);
        } catch (Exception e) {
            toast("打不开目录选择器：" + e.getMessage());
        }
    }

    // ---- v0.20 二维码配对：出码（接收端）与扫码（发送端）----

    public void openScanner() {
        try {
            activity.startActivityForResult(new Intent(activity, ScanActivity.class), REQ_SCAN);
        } catch (Exception e) {
            toast("打不开扫码页：" + e.getMessage());
        }
    }

    /** 扫码结果：解析与校验都在 Rust 里做，这里只负责把目标与令牌填进界面。 */
    private void handleScanResult(String payload) {
        if (payload == null || payload.isEmpty()) {
            return;
        }
        JSONObject o;
        try {
            String parsed = TcNative.pairParse(payload);
            o = new JSONObject(parsed == null ? "{}" : parsed);
        } catch (Exception e) {
            toast("二维码解析失败");
            return;
        }
        if (!o.optBoolean("ok")) {
            toast("这不是通传的配对二维码：" + o.optString("reason", "无法识别"));
            return;
        }
        if (o.optBoolean("expired")) {
            toast("二维码已过期，请让对方重新生成");
            return;
        }
        String addr = o.optString("addr");
        String fp = o.optString("fp");
        String code = o.optString("code");
        String name = o.optString("name");
        // 令牌先填：紧随其后的落点查询会带它走同一场 TLS 会话，
        // 接收端据此直接把这台设备记进信任表
        ui.setPairCode(code == null ? "" : code);
        applyScannedPeer(addr, fp, name);
        String hint = o.optString("hint", "");
        String label = name.isEmpty() ? addr : name;
        toast("已配对：" + label + (hint.isEmpty() ? "，选好文件即可发送" : "（" + hint + "）"));
    }

    /** 扫码得到的目标直接覆盖已选列表并落盘：下次进应用默认还是它。 */
    private void applyScannedPeer(String addr, String fp, String name) {
        selectedPeers.clear();
        selectedPeers.add(new String[]{addr, fp, name});
        targetAddr = addr;
        targetFp = fp;
        targetName = name;
        activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE).edit()
                .putString("send_addr", addr)
                .putString("send_fp", fp)
                .putString("send_name", name)
                .apply();
        updateTargetLabel();
    }

    /**
     * 出码对话框。先看有没有仍然有效的码：关掉窗口不等于撤销，
     * 重新打开时沿用同一张，避免用户只是切了一下界面就把对方的码作废。
     */
    public void showPairDialog() {
        // 出码意味着「我现在能接收」；常驻没开就顺手拉起，免得对方扫完却连不上
        if (!ReceiverService.isRunning()) {
            startRecv();
        }
        ui.setPairDialogVisible(true);
        ui.setPairKeepScreenOn(true);
        // 打开即出码：沿用仍有效的一张，没有就新发一张。
        // 之前只设可见、没调这里，弹窗打开只会显示「已过期」，得手动点「重新生成」。
        issuePairCode();
    }

    /** 对话框内「重新生成」。 */
    public void regenPairCode() {
        issuePairCode(true);
    }

    /** 对话框内「立即作废」。 */
    public void revokePairCode() {
        final String dir = ReceiverService.identityDir(activity).getAbsolutePath();
        new Thread(() -> {
            TcNative.pairRevoke(dir);
            activity.runOnUiThread(() -> {
                ui.setPairExpiresAt(0L);
                ui.setPairQr(null);
                ui.setPairInfo("已作废：任何设备都不能再凭这张码配对");
            });
        }, "tc-pair").start();
    }

    public void dismissPairDialog() {
        ui.setPairDialogVisible(false);
        ui.setPairKeepScreenOn(false);
    }

    /** 打开对话框与「重新生成」共用：先查有效码，没有就新发一张。 */
    public void issuePairCode() {
        issuePairCode(false);
    }

    private void issuePairCode(boolean forceNew) {
        final String dir = ReceiverService.identityDir(activity).getAbsolutePath();
        final String name = "手机-" + Build.MODEL;
        final android.content.SharedPreferences prefs =
                activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE);
        new Thread(() -> {
            String saved = ui.getPairAddr().isEmpty()
                    ? prefs.getString("pair_addr", "") : ui.getPairAddr();
            String st = TcNative.pairStatus(dir, saved, name);
            boolean active = !forceNew && st != null && st.contains("\"active\":true");
            String json = active ? st : TcNative.pairIssue(dir, saved, name);
            activity.runOnUiThread(() -> renderPairJson(json));
        }, "tc-pair").start();
    }

    /** 配对码 / 一次性令牌：界面输入，发送时随清单带给对端。 */
    public void setPairCode(String code) {
        // 手输时容易带前后空格；旧版是在读取时 trim()，这里在写入时就去掉，
        // 免得落点查询和发送拿到脏令牌、提示只说「配对码不对」难以定位。
        ui.setPairCode(code == null ? "" : code.trim());
    }

    public void savePairAddr(String addr) {
        ui.setPairAddr(addr);
        activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE)
                .edit().putString("pair_addr", addr).apply();
    }

    /** 把 Rust 返回的配对 JSON 画成二维码与倒计时状态。 */
    private void renderPairJson(String json) {
        try {
            JSONObject o = new JSONObject(json == null ? "{}" : json);
            if (!o.optBoolean("ok") || !o.optBoolean("active")) {
                ui.setPairExpiresAt(0L);
                ui.setPairQr(null);
                ui.setPairInfo(o.optString("reason", "当前没有有效二维码"));
                return;
            }
            String addr = o.optString("addr", "");
            if (!addr.isEmpty()) {
                ui.setPairAddr(addr);
            }
            Bitmap bmp = qrBitmap(o.optString("payload"), dp(220));
            if (bmp == null) {
                ui.setPairInfo("二维码生成失败");
                return;
            }
            ui.setPairQr(bmp);
            ui.setPairExpiresAt(o.optLong("expires"));
            ui.setPairInfo("");
        } catch (Exception e) {
            ui.setPairInfo("二维码数据解析失败");
        }
    }

    /** 用 zxing 把配对文本画成位图。M 级纠错：屏幕直扫，容得下轻微反光与摩尔纹。 */
    private static Bitmap qrBitmap(String text, int size) {
        if (text == null || text.isEmpty() || size <= 0) {
            return null;
        }
        try {
            Map<com.google.zxing.EncodeHintType, Object> hints =
                    new EnumMap<>(com.google.zxing.EncodeHintType.class);
            hints.put(com.google.zxing.EncodeHintType.CHARACTER_SET, "UTF-8");
            hints.put(com.google.zxing.EncodeHintType.MARGIN, 1);
            hints.put(com.google.zxing.EncodeHintType.ERROR_CORRECTION,
                    com.google.zxing.qrcode.decoder.ErrorCorrectionLevel.M);
            com.google.zxing.common.BitMatrix m = new com.google.zxing.qrcode.QRCodeWriter()
                    .encode(text, com.google.zxing.BarcodeFormat.QR_CODE, size, size, hints);
            int[] px = new int[size * size];
            for (int y = 0; y < size; y++) {
                for (int x = 0; x < size; x++) {
                    px[y * size + x] = m.get(x, y) ? 0xFF000000 : 0xFFFFFFFF;
                }
            }
            return Bitmap.createBitmap(px, size, size, Bitmap.Config.ARGB_8888);
        } catch (Exception e) {
            return null;
        }
    }

    /** 配对倒计时由 Compose 侧用 LaunchedEffect 每秒读 pairExpiresAt 渲染，这里只给原始值。 */

    // ---- 发送 ----

    /** 发送：按已选目标逐个发，后台线程跑会话，另一个线程每 250ms 把进度刷到界面。 */
    public void startSend() {
        // 只记「有几台目标、选了几项」，不记任何路径或文件名
        android.util.Log.i("TONGCHUAN-SEND",
                "start targets=" + selectedPeers.size() + " picked=" + pickedPaths.size());
        if (selectedPeers.isEmpty()) {
            ui.setSendProgressText("请先「发现附近的设备」并选一个目标");
            toast("请先「发现附近的设备」并选一个目标");
            return;
        }
        if (pickedPaths.isEmpty()) {
            ui.setSendProgressText("请先选择文件或文件夹");
            toast("请先选择文件或文件夹");
            return;
        }
        JSONArray arr = new JSONArray();
        for (String p : pickedPaths) {
            arr.put(p);
        }
        // 快照：发送过程中用户继续勾选/取消，不该改变这一批的目标
        final List<String[]> targets = new ArrayList<>(selectedPeers);
        // v0.16：落点也在开跑前定死，发送期间换目标不影响这一批
        final String batchDest = selectedPeers.size() == 1 ? destId : "";
        final String pathsJson = arr.toString();
        // M4：配对码由用户现场填，core 早已支持 pair_code，之前手机侧没有入口
        final String pairCode = ui.getPairCode();
        final boolean force = ui.getForce();
        final String identityDir = ReceiverService.identityDir(activity).getAbsolutePath();
        if (SendService.isRunning()) {
            toast("上一批还在发送，请等它结束");
            return;
        }
        // 前台服务必须有通知权限，否则 Android 13+ 直接拒绝启动
        ensureNotificationPermission();

        // 每一批都是新的控制状态：上一批的暂停/取消不能带过来
        sendPaused = false;
        sendAborted = false;
        SendService.paused = false;
        ui.setSendPaused(false);
        ui.setSendCancelling(false);
        ui.setSendRunning(true);
        ui.setSendProgress(0);
        ui.setSendIndeterminate(true);
        ui.setSendProgressText("正在扫描并计算校验值…（文件多时需要一会儿）");

        new Thread(() -> {
            final boolean[] running = {true};
            Thread poller = new Thread(() -> {
                long netT0 = 0;
                long lastNet = 0;
                while (running[0]) {
                    String p = TcNative.sendProgress();
                    boolean active = p != null && p.contains("\"active\":true");
                    if (active) {
                        int percent = 0;
                        String text = "";
                        boolean knowTotal = false;
                        long speedBps = 0;
                        long bytesSent = 0;
                        long total = 0;
                        try {
                            JSONObject o = new JSONObject(p);
                            String phase = o.optString("phase", "sending");
                            total = o.optLong("totalBytes");
                            bytesSent = o.optLong("bytesSent");
                            speedBps = o.optLong("speedBps");
                            knowTotal = total > 0 && !"scanning".equals(phase);
                            if (knowTotal) {
                                percent = o.optInt("percent", 0);
                                text = percent + "%  " + human(bytesSent)
                                        + " / " + human(total) + "  " + human(speedBps) + "/s";
                            } else {
                                text = "正在扫描：已处理 " + o.optLong("scanFiles")
                                        + " 个文件 / " + human(o.optLong("scanBytes"));
                            }
                            if (sendPaused) {
                                text = "已暂停 · " + text;
                            }
                        } catch (Exception ignored) {
                        }
                        long now = System.currentTimeMillis();
                        if (knowTotal && now - lastNet >= 1000) {
                            if (netT0 == 0) {
                                netT0 = now;
                            }
                            lastNet = now;
                            android.util.Log.i("TONGCHUAN-SEND", "NET t=" + ((now - netT0) / 1000)
                                    + "s " + wifiSample() + " speed=" + human(speedBps) + "/s"
                                    + " sent=" + bytesSent + "/" + total);
                        }
                        final int pc = percent;
                        final String tx = text;
                        final boolean known = knowTotal;
                        activity.runOnUiThread(() -> {
                            if (known) {
                                ui.setSendIndeterminate(false);
                            }
                            ui.setSendProgress(pc);
                            ui.setSendProgressText(tx);
                        });
                    }
                    try {
                        Thread.sleep(250);
                    } catch (InterruptedException e) {
                        return;
                    }
                }
            }, "tc-send-progress");
            poller.start();

            int okCount = 0;
            long totalFiles = 0;
            long totalBytes = 0;
            String lastText = "";
            List<String> failures = new ArrayList<>();
            for (int i = 0; i < targets.size(); i++) {
                // 取消后不再开始后面的目标：否则用户点了取消，下一台又连上来，观感像没理他
                if (sendAborted) {
                    break;
                }
                String[] t = targets.get(i);
                String who = t[2].isEmpty() ? t[0] : t[2];
                final String progress = targets.size() > 1
                        ? "正在发送 " + (i + 1) + "/" + targets.size() + " → " + who
                        : "正在扫描并计算校验值…（文件多时需要一会儿）";
                activity.runOnUiThread(() -> ui.setSendProgressText(progress));

                SendService.start(activity, t[0], t[1], pathsJson, pairCode, identityDir,
                        Build.MODEL, force, batchDest);
                String out = awaitSendResult();
                Outcome oc = parseOutcome(t[0], t[2], out);
                if (oc.ok) {
                    okCount++;
                    totalFiles += oc.files;
                    totalBytes += oc.bytes;
                } else {
                    failures.add(who + "：" + oc.text);
                }
                lastText = oc.text;
                activity.runOnUiThread(this::refreshSendHistory);
            }
            running[0] = false;

            final String summary;
            if (sendAborted) {
                summary = "已取消：已完成 " + okCount + "/" + targets.size() + " 台"
                        + (okCount > 0 ? "，" + totalFiles + " 个文件 / " + human(totalBytes) : "");
            } else if (targets.size() == 1) {
                summary = lastText;
            } else if (failures.isEmpty()) {
                summary = "全部完成：" + okCount + " 台，" + totalFiles + " 个文件，"
                        + human(totalBytes);
            } else {
                summary = "完成 " + okCount + "/" + targets.size() + " 台；失败：\n"
                        + String.join("\n", failures);
            }
            activity.runOnUiThread(() -> {
                // 小文件可能在「扫描」阶段就传完了，进度条从没切成确定态；
                // 收尾时必须显式停掉滚动并拉满，否则看起来像还在传
                ui.setSendIndeterminate(false);
                ui.setSendProgress(100);
                ui.setSendProgressText(summary);
                ui.setSendRunning(false);
                ui.setSendPaused(false);
                ui.setSendCancelling(false);
                sendPaused = false;
                SendService.paused = false;
                refreshSendHistory();
            });
        }, "tc-send").start();
    }

    /** 暂停 / 继续当前发送：状态只在块边界生效，点完立刻改状态，不让用户以为没反应。 */
    public void togglePauseSend() {
        if (!SendService.isRunning() || sendAborted) {
            return;
        }
        sendPaused = !sendPaused;
        TcNative.sendPause(sendPaused);
        SendService.paused = sendPaused;
        ui.setSendPaused(sendPaused);
        toast(sendPaused ? "已暂停，点「继续」接着传" : "已继续发送");
    }

    /** 取消当前发送：接收端会留下 .tcpart，下次同名发送可以续传。 */
    public void cancelSend() {
        if (!SendService.isRunning()) {
            return;
        }
        ui.setCancelConfirmVisible(true);
    }

    public void confirmCancelSend() {
        ui.setCancelConfirmVisible(false);
        sendAborted = true;
        sendPaused = false;
        SendService.paused = false;
        ui.setSendPaused(false);
        ui.setSendCancelling(true);
        TcNative.sendCancel();
        ui.setSendProgressText("正在取消…");
    }

    public void dismissCancelDialog() {
        ui.setCancelConfirmVisible(false);
    }

    /** 等服务交回一条结果；服务没起来/被系统终止时按失败返回，避免界面永远停在「正在发送」。 */
    private String awaitSendResult() {
        long started = System.currentTimeMillis();
        while (true) {
            String out = SendService.takeResult();
            if (out != null) {
                return out;
            }
            if (!SendService.isRunning() && System.currentTimeMillis() - started > 3000) {
                return "{\"ok\":false,\"reason\":\"发送服务未能启动或被系统终止\"}";
            }
            try {
                Thread.sleep(200);
            } catch (InterruptedException e) {
                return "{\"ok\":false,\"reason\":\"发送被中断\"}";
            }
        }
    }

    /** 一条发送结果：既给界面用一句话，也落一条手机侧历史。 */
    private static final class Outcome {
        boolean ok;
        long files;
        long bytes;
        /** v0.12：接收端按内容跳过的文件数，完成提示要分「新传 / 跳过」两个口径 */
        long skipped;
        String text = "";
    }

    private Outcome parseOutcome(String targetAddr, String targetName, String out) {
        Outcome r = new Outcome();
        try {
            JSONObject o = new JSONObject(out == null ? "{}" : out);
            r.ok = o.optBoolean("ok");
            r.files = o.optLong("files");
            r.bytes = o.optLong("bytes");
            r.skipped = o.optLong("skipped");
            String reason = o.optString("reason", "");
            // 日志脱敏：含 "/" 的片段一律替换，避免把路径写进日志
            String safe = reason.replaceAll("[^\\s]*/[^\\s]*", "<path>");
            android.util.Log.i("TONGCHUAN-SEND",
                    "done ok=" + r.ok + " files=" + r.files
                            + " bytes=" + r.bytes + " reason=" + safe);
            StringBuilder dest = new StringBuilder();
            JSONArray arr = o.optJSONArray("dest");
            if (arr != null) {
                for (int i = 0; i < arr.length(); i++) {
                    if (i > 0) {
                        dest.append("、");
                    }
                    dest.append(arr.optString(i));
                }
            }
            String who = targetName == null || targetName.isEmpty() ? targetAddr : targetName;
            String newCount = "发送完成：新传 " + Math.max(0, r.files - r.skipped) + " 个";
            if (r.skipped > 0) {
                newCount += " / 跳过 " + r.skipped + " 个";
            }
            r.text = r.ok
                    ? newCount + "，" + human(r.bytes)
                    + (dest.length() > 0 ? "\n落点：" + who + " 的「" + dest + "」文件夹" : "")
                    : friendlyFailure(reason);
            SendLog.Record rec = new SendLog.Record();
            rec.ts = System.currentTimeMillis() / 1000;
            rec.name = targetName;
            rec.addr = targetAddr;
            rec.files = r.files;
            rec.bytes = r.bytes;
            rec.ok = r.ok;
            rec.dest = dest.toString();
            rec.skipped = r.skipped;
            rec.reason = r.ok ? "" : reason;
            SendLog.append(activity, rec);
        } catch (Exception e) {
            r.text = "发送结果解析失败：" + e.getMessage();
        }
        return r;
    }

    /** 最近发送：只显示最近 5 条，最新的在上；不显示文件路径与文件名。 */
    private void refreshSendHistory() {
        List<SendLog.Record> list = SendLog.recent(activity, 5);
        List<HistoryItem> items = new ArrayList<>();
        if (list.isEmpty()) {
            ui.setHistory(items);
            return;
        }
        for (SendLog.Record r : list) {
            String who = r.name.isEmpty() ? r.addr : r.name;
            String text = timeLabel(r.ts) + " · " + who + " · "
                    + (r.ok ? "新传 " + Math.max(0, r.files - r.skipped) + " 个"
                    + (r.skipped > 0 ? " / 跳过 " + r.skipped + " 个" : "")
                    + " / " + human(r.bytes)
                    + (r.dest.isEmpty() ? "" : " → " + r.dest) + " · 成功"
                    : "失败：" + firstLine(friendlyFailure(r.reason)));
            items.add(new HistoryItem(r.ts, who, r.ok, text));
        }
        ui.setHistory(items);
    }

    private static String firstLine(String s) {
        int i = s.indexOf('\n');
        return i < 0 ? s : s.substring(0, i);
    }

    // ---- v0.8 剪贴板同步 ----

    /** 读一项偏好；不存在时返回空串。 */
    private String pref(String key) {
        return activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE)
                .getString(key, "");
    }

    private String pref(String key, String def) {
        return activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE)
                .getString(key, def);
    }

    private boolean prefBool(String key, boolean def) {
        return activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE)
                .getBoolean(key, def);
    }

    /** v1.2.0：切换「安全」总开关并持久化。关只收起管理入口，不放开接收信任门禁。 */
    public void setSecurityEnabled(boolean enabled) {
        activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE)
                .edit().putBoolean("security_enabled", enabled).apply();
        ui.setSecurityEnabled(enabled);
    }

    /**
     * 把内核的英文原因翻成「怎么了 + 下一步」。
     * 用户不该为了看懂一次失败去翻 logcat。
     */
    private String friendlyFailure(String reason) {
        return friendlyFailure(reason, selfFp);
    }

    /** 纯函数版：单测直接调它，不必构造控制器。 */
    public static String friendlyFailure(String reason, String selfFp) {
        String r = reason == null ? "" : reason;
        String lr = r.toLowerCase(Locale.getDefault());
        String mine = selfFp == null ? "" : selfFp;
        if (lr.contains("untrusted")) {
            return "对方还没有信任这台手机。\n"
                    + "下一步：去 NAS 管理页的「信任设备」里批准下面这串指纹；"
                    + "或在上面填对方设置的配对码后重试。\n"
                    + "本机指纹：" + fp8(mine);
        }
        if (lr.contains("pair code")) {
            return "配对码不对。\n下一步：确认对方配置里的 6 位数字，重新填写后再发。";
        }
        if (lr.contains("block size") || lr.contains("version mismatch")) {
            return "两端版本不一致，暂时不能互通。\n"
                    + "下一步：把手机、电脑、NAS 升到同一版本（0.5.x 之间也要看块大小是否一致）。\n"
                    + "原始提示：" + r;
        }
        if (lr.contains("os error") || lr.contains("timed out")
                || lr.contains("connection") || lr.contains("reset")) {
            return "连接中断：对方可能已关闭接收、切了网络，或不在同一个 Wi-Fi。\n"
                    + "下一步：确认对方「常驻接收」在运行，再重试。\n"
                    + "原始提示：" + r;
        }
        return "发送失败：" + (r.isEmpty() ? "未知原因" : r);
    }

    /** 列出接收目录里最近的文件（最多 20 条），点一下交给系统打开。 */
    private void refreshRecent() {
        // v0.12：先把「最近一次接收」的结果放在最上面
        String last = activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE)
                .getString("last_recv", "");
        String lastText = "";
        if (!last.isEmpty()) {
            String[] f = last.split("\\|", -1);
            if (f.length >= 7) {
                String device = f[0].isEmpty() ? "未知设备" : f[0];
                String roots = f[1].isEmpty() ? "接收目录" : f[1];
                long skipped = parseLong(f[2]);
                long files = parseLong(f[3]);
                long bytes = parseLong(f[4]);
                boolean interrupted = "1".equals(f[6]);
                lastText = "最近一次：" + device + " · 落点「" + roots + "」 · 新收 "
                        + Math.max(0, files - skipped) + " 个"
                        + (skipped > 0 ? " / 跳过 " + skipped + " 个" : "")
                        + " · " + human(bytes) + (interrupted ? " · 中断" : "");
            }
        }
        ui.setLastRecvText(lastText);

        List<File> files = new ArrayList<>();
        collect(Dest.current(activity), files, 0);
        files.sort((a, b) -> Long.compare(b.lastModified(), a.lastModified()));
        List<RecentItem> items = new ArrayList<>();
        SimpleDateFormat fmt = new SimpleDateFormat("MM-dd HH:mm", Locale.getDefault());
        int n = Math.min(20, files.size());
        for (int i = 0; i < n; i++) {
            File f = files.get(i);
            items.add(new RecentItem(f, f.getName(), human(f.length()),
                    fmt.format(new Date(f.lastModified()))));
        }
        ui.setRecent(items);
    }

    /** 只扫 3 层：接收目录通常是「根/目录名/文件」，再深就该交给文件管理器了。 */
    private void collect(File dir, List<File> out, int depth) {
        if (depth > 3) {
            return;
        }
        File[] list = dir.listFiles();
        if (list == null) {
            return;
        }
        for (File f : list) {
            if (f.isDirectory()) {
                collect(f, out, depth + 1);
            } else if (!f.getName().endsWith(".tcpart") && !f.getName().endsWith(".tcpart.map")) {
                // 半成品和位图是内部状态，不展示给用户
                out.add(f);
            }
        }
    }

    public void openReceived(File f) {
        if (f == null || !f.exists()) {
            // 接收目录被系统清理/用户挪走时，先别报「找不到应用」，直接刷新列表
            toast("文件已经不在接收目录里了，刷新一下列表看看");
            refreshRecent();
            return;
        }
        Uri uri = FileServeProvider.uriFor(activity, f);
        String type = activity.getContentResolver().getType(uri);
        if (type == null || type.isEmpty()) {
            type = "*/*";
        }
        Intent i = new Intent(Intent.ACTION_VIEW);
        i.setDataAndType(uri, type);
        i.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        try {
            activity.startActivity(i);
        } catch (android.content.ActivityNotFoundException e) {
            // 没有任何应用认领这个类型时明确告知，而不是吞掉异常让人以为点了没反应
            toast("没有能打开「" + f.getName() + "」的应用，可以先把文件移到「下载」再试");
        } catch (Exception e) {
            toast("打开失败：" + e.getMessage());
        }
    }

    /**
     * 传一次 Wi-Fi 状态，把「信号强度 / 协商速率」和实测吞吐记进同一条日志。
     * BSSID 在 Android 12+ 没有定位权限时会被系统脱敏成 02:00:00:00:00:00，照记即可。
     */
    private String wifiSample() {
        try {
            android.net.wifi.WifiManager wm = (android.net.wifi.WifiManager)
                    activity.getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            if (wm == null) {
                return "rssi=? tx=? bssid=?";
            }
            android.net.wifi.WifiInfo info = wm.getConnectionInfo();
            if (info == null) {
                return "rssi=? tx=? bssid=?";
            }
            int tx = Build.VERSION.SDK_INT >= 29 ? info.getTxLinkSpeedMbps() : -1;
            String bssid = info.getBSSID();
            return "rssi=" + info.getRssi() + " tx=" + tx + " bssid=" + (bssid == null ? "?" : bssid);
        } catch (Exception e) {
            return "rssi=? tx=? bssid=?";
        }
    }

    private static long parseLong(String s) {
        try {
            return Long.parseLong(s.trim());
        } catch (Exception e) {
            return 0;
        }
    }

    /** 字节数 human-readable；历史与接收列表共用同一口径。 */
    public static String human(long n) {
        Locale l = Locale.getDefault();
        if (n < 1024) {
            return n + " B";
        }
        if (n < 1024L * 1024) {
            return String.format(l, "%.1f KB", n / 1024.0);
        }
        if (n < 1024L * 1024 * 1024) {
            return String.format(l, "%.1f MB", n / 1048576.0);
        }
        return String.format(l, "%.2f GB", n / 1073741824.0);
    }

    /**
     * 手机才是落盘入口，未批准的电脑第一次投递会被门禁挡下。
     * 这里列「待确认」和「已信任」：批准/撤销直接写 trust.jsonl，
     * 下一场会话立即生效（门禁每次判定都重放信任表），不需要重启接收服务。
     */
    private void refreshTrust() {
        String json = TcNative.trustJson(ReceiverService.identityDir(activity).getAbsolutePath());
        if (json == null) {
            List<TrustItem> single = new ArrayList<>();
            single.add(new TrustItem(false, "信任列表读取失败", ""));
            ui.setTrust(single);
            return;
        }
        try {
            JSONObject o = new JSONObject(json);
            String mine = o.optString("selfFp", "");
            if (!mine.isEmpty()) {
                selfFp = mine;
                ui.setSelfFpText("本机指纹：" + fp8(mine) + "（对方批准时核对这串）");
            }
            JSONArray pending = o.optJSONArray("pending");
            JSONArray peers = o.optJSONArray("peers");
            int p = pending == null ? 0 : pending.length();
            int k = peers == null ? 0 : peers.length();
            List<TrustItem> items = new ArrayList<>();
            if (p == 0 && k == 0) {
                items.add(new TrustItem(false,
                        "还没有信任任何电脑。电脑第一次投递会被拒绝，批准后即可正常收。", ""));
                ui.setTrust(items);
                return;
            }
            for (int i = 0; i < p; i++) {
                JSONObject e = pending.getJSONObject(i);
                String label = "待确认：" + deviceLabel(e.optString("name"), e.optString("platform"))
                        + " · " + fp8(e.optString("fp")) + " · " + timeLabel(e.optLong("ts"));
                items.add(new TrustItem(true, label, e.optString("fp")));
            }
            for (int i = 0; i < k; i++) {
                JSONObject e = peers.getJSONObject(i);
                String alias = e.optString("alias");
                String label = "已信任：" + deviceLabel(
                        alias.isEmpty() ? e.optString("name") : alias,
                        e.optString("platform")) + " · " + fp8(e.optString("fp"));
                items.add(new TrustItem(false, label, e.optString("fp")));
            }
            ui.setTrust(items);
        } catch (Exception e) {
            List<TrustItem> single = new ArrayList<>();
            single.add(new TrustItem(false, "信任列表解析失败：" + e.getMessage(), ""));
            ui.setTrust(single);
        }
    }

    public void approve(String fp) {
        boolean ok = TcNative.trustApprove(
                ReceiverService.identityDir(activity).getAbsolutePath(), fp);
        toast(ok ? "已批准，这台电脑可以投递了" : "批准失败，请重试");
        refreshTrust();
    }

    public void revoke(String fp) {
        TcNative.trustRevoke(ReceiverService.identityDir(activity).getAbsolutePath(), fp);
        toast("已撤销，这台电脑下次投递会被拒绝");
        refreshTrust();
    }

    /** 指纹只展示前 8 位：够用户区分设备，又不会把整屏占满。 */
    private static String fp8(String fp) {
        return fp.length() > 8 ? fp.substring(0, 8) + "…" : fp;
    }

    private String deviceLabel(String name, String platform) {
        if (name == null || name.isEmpty()) {
            return platform == null || platform.isEmpty() ? "未知设备" : platform;
        }
        return name;
    }

    private String timeLabel(long secs) {
        if (secs <= 0) {
            return "-";
        }
        return new SimpleDateFormat("MM-dd HH:mm", Locale.getDefault())
                .format(new Date(secs * 1000L));
    }

    private String versionName() {
        try {
            return activity.getPackageManager()
                    .getPackageInfo(activity.getPackageName(), 0).versionName;
        } catch (Exception e) {
            return "";
        }
    }

    private void toast(String s) {
        android.widget.Toast.makeText(activity, s, android.widget.Toast.LENGTH_SHORT).show();
    }

    /** 顶部状态行：运行状态 + 端口 + 保活 + 权限。 */
    private void refresh() {
        StringBuilder sb = new StringBuilder();
        boolean svcRunning = ReceiverService.isRunning();
        boolean ready = ReceiverService.isReceiverReady();
        String err = ReceiverService.lastError();
        // 服务活着不等于端口在听：旧版只看服务存活，端口被占用时会显示「运行中」，
        // 实际对方根本连不上、发现列表里也没有这台手机（2026-11-28 真机复现）。
        if (ready) {
            sb.append("常驻：运行中");
        } else if (svcRunning && err != null && !err.isEmpty()) {
            sb.append("常驻：启动失败");
        } else if (svcRunning) {
            sb.append("常驻：正在启动…");
        } else {
            sb.append("常驻：未启动");
        }
        if (err != null && !err.isEmpty()) {
            sb.append("\n").append(err);
        }
        sb.append("\n端口：53318（TCP + UDP）");
        sb.append("\n保活增强：").append(KeepAliveService.isEnabled(activity) ? "已开启" : "未开启");
        sb.append("\n所有文件访问：").append(Dest.hasAllFilesAccess() ? "已授权" : "未授权（只能存应用私有目录）");
        ui.setStatusText(sb.toString());
        ui.setStatusOk(ready);
        ui.setRecvRunning(svcRunning);
        ui.setRecvPending(recvPending);
        ui.setKeepAliveEnabled(KeepAliveService.isEnabled(activity));
        ui.setHasAllFilesAccess(Dest.hasAllFilesAccess());
        ui.setReceiveDirText(receiveDirText());
        // v0.17.1：状态刷新必须联动信任列表，否则被拒后的「待确认」要退出重进才出现。
        // 迁移到 Compose 时漏了这条，这里补回。
        refreshTrust();
    }

    /**
     * v1.1.0：接收页实时进度。数据源是内核进程级原子（TcNative.recvProgress）。
     * 接收中显示「已收 / 总量 · 速度」，不接收时清空；解析失败不抛，保持界面稳定。
     */
    private void refreshRecvProgress() {
        // v1.4.1：接收记录变了就刷新列表。比对 last_recv 比看 active 跳变可靠——
        // 几百毫秒的小传输在 1 秒轮询里根本看不到「在收」，原来的做法会漏刷。
        String lr = activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE)
                .getString("last_recv", "");
        if (lr != null && !lr.equals(seenLastRecv)) {
            boolean first = seenLastRecv == null;
            seenLastRecv = lr;
            if (!first && !lr.isEmpty()) {
                refreshRecent();
            }
        }
        if (!ReceiverService.isRunning()) {
            ui.setRecvReceiving(false);
            ui.setRecvProgress(0);
            ui.setRecvSpeedText("");
            ui.setRecvProgressText("");
            // v1.4.1：接收刚结束就把列表刷新出来，不用等用户点通知回应用
            if (wasReceiving) {
                wasReceiving = false;
                refreshRecent();
            }
            return;
        }
        String p = TcNative.recvProgress();
        if (p == null) {
            ui.setRecvReceiving(false);
            return;
        }
        try {
            JSONObject o = new JSONObject(p);
            boolean active = o.optBoolean("active", false);
            ui.setRecvReceiving(active);
            // v1.4.1：从「在收」变「收完」的那一拍刷新「最近接收」。
            // 原来只有 onResume/refreshAll 会扫目录，前台收完文件列表一直是旧的。
            if (active) {
                wasReceiving = true;
            } else if (wasReceiving) {
                wasReceiving = false;
                refreshRecent();
            }
            if (active) {
                ui.setRecvProgress(o.optInt("percent", 0));
                long br = o.optLong("bytesReceived");
                long tb = o.optLong("totalBytes");
                long bps = o.optLong("speedBps");
                // bps 为 0 说明还在首个采样窗口内（或刚好停住），显示「计算中」而不是 0 B/s
                ui.setRecvSpeedText(bps > 0 ? human(bps) + "/s" : "计算中…");
                ui.setRecvProgressText("已收 " + human(br) + (tb > 0 ? " / " + human(tb) : ""));
            } else {
                ui.setRecvSpeedText("");
                ui.setRecvProgressText("");
            }
        } catch (Exception ignored) {
            // 解析失败保持上次显示，不清空以免闪烁
        }
    }

    /**
     * v0.12：打开应用即自动常驻，包括后台保活。
     * 接收服务可以静默拉起；无障碍授权只能由用户在系统设置里勾选，
     * 电池白名单也只能走系统对话框，所以这两项只在首次安装/升级后引导一次。
     */
    private void autoKeepAlive() {
        setWantRunning(true);
        if (!ReceiverService.isRunning()) {
            ensureNotificationPermission();
            ReceiverService.start(activity);
            uiTick.postDelayed(this::refresh, 800);
            uiTick.postDelayed(this::refresh, 2500);
        } else if (!ReceiverService.isReceiverReady()) {
            // 服务在、端口没听（上次 bind 失败留下的半死状态）：重开一次，
            // 不能因为「服务已存在」就跳过，否则列表里永远看不到这台手机。
            ReceiverService.restart(activity);
            uiTick.postDelayed(this::refresh, 800);
            uiTick.postDelayed(this::refresh, 2500);
        }
        android.content.SharedPreferences p =
                activity.getSharedPreferences(Dest.PREFS, Activity.MODE_PRIVATE);
        if (p.getBoolean("auto_keepalive_done", false)) {
            return;
        }
        p.edit().putBoolean("auto_keepalive_done", true).apply();
        // 电池白名单：系统弹窗确认，应用不能替用户点
        try {
            Intent i = new Intent(
                    android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS);
            i.setData(Uri.parse("package:" + activity.getPackageName()));
            activity.startActivity(i);
        } catch (Exception ignored) {
            // 个别 ROM 没有这个页面：不影响接收
        }
        if (!KeepAliveService.isEnabled(activity)) {
            toast("已自动开启常驻接收；后台保活请勾选「通传」无障碍服务（只需一次）");
            try {
                activity.startActivity(
                        new Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS));
            } catch (Exception ignored) {
            }
        }
    }

    /** 记「用户希望常驻」这个意图，开机自启只认这个开关，不认服务当前是否活着。 */
    private void setWantRunning(boolean want) {
        activity.getSharedPreferences(BootReceiver.PREFS, Activity.MODE_PRIVATE)
                .edit().putBoolean(BootReceiver.KEY_WANT, want).apply();
    }

    /**
     * 启停按钮的即时反馈：先改状态文案，再每 300ms 看一眼真实状态，最多等 5 秒。
     * 之前点完只刷新一次，服务还没起来就显示「未启动」，看起来像按钮没生效。
     */
    private void waitRecvState(boolean want) {
        // 启动要在端口重试窗口（最多 6 次 × 2s）内等，否则还在重试就先报「没等到」
        final long deadline = System.currentTimeMillis() + (want ? 14000 : 5000);
        final Runnable[] tick = new Runnable[1];
        tick[0] = () -> {
            boolean stateOk = want ? ReceiverService.isReceiverReady()
                    : !ReceiverService.isRunning();
            if (stateOk) {
                recvPending = false;
                ui.setRecvPending(false);
                refresh();
                toast(want ? "常驻接收已启动" : "常驻接收已停止");
                return;
            }
            if (System.currentTimeMillis() > deadline) {
                recvPending = false;
                ui.setRecvPending(false);
                refresh();
                toast("没等到接收端就绪；失败原因见上方提示，可点「停止常驻接收」再重新启动");
                return;
            }
            uiTick.postDelayed(tick[0], 300);
        };
        uiTick.postDelayed(tick[0], 300);
    }

    private void ensureNotificationPermission() {
        if (Build.VERSION.SDK_INT >= 33
                && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED) {
            activity.requestPermissions(
                    new String[]{Manifest.permission.POST_NOTIFICATIONS}, 1);
        }
    }

    private int dp(int v) {
        return Math.round(v * activity.getResources().getDisplayMetrics().density);
    }
}
