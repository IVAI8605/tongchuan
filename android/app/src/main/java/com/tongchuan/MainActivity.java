package com.tongchuan;

import android.Manifest;
import android.content.ClipData;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.os.Build;
import android.os.Bundle;
import android.text.InputType;
import android.view.Gravity;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;

import android.app.Activity;
import android.net.Uri;

import org.json.JSONArray;
import org.json.JSONObject;

import java.io.File;
import java.text.SimpleDateFormat;
import java.util.ArrayList;
import java.util.Date;
import java.util.List;
import java.util.Locale;

/**
 * 极简主界面：开关常驻、看状态与落点、打开刚收到的文件，
 * 以及批准/撤销哪些电脑可以往本机投递。
 */
public class MainActivity extends Activity {

    private static final int REQ_TREE = 100;
    private static final int REQ_SEND_FILES = 101;
    private static final int REQ_SEND_TREE = 102;
    /** v0.20 扫码配对：从 ScanActivity 拿回二维码文本 */
    private static final int REQ_SCAN = 103;
    /** v0.13.4：配对码并入后统一的面板标题，展开/收起文案都从这里取 */
    private static final String DEVICE_TITLE = "设备与保活（发现、授权、信任、配对码）";

    // v0.13 视觉基调：浅灰底 + 白卡片 + 一个深绿色主色。
    // 全部代码构建界面，颜色集中在这里，改主题不用满文件找字面量。
    private static final int COLOR_BG = Color.parseColor("#F4F5F3");
    private static final int COLOR_CARD = Color.parseColor("#FFFFFF");
    private static final int COLOR_INK = Color.parseColor("#1B1B1B");
    private static final int COLOR_MUTED = Color.parseColor("#6B6B6B");
    private static final int COLOR_ACCENT = Color.parseColor("#1F6F5C");
    private static final int COLOR_OK = Color.parseColor("#2E7D32");
    private static final int COLOR_WARN = Color.parseColor("#9A5B23");
    private static final int COLOR_ERR = Color.parseColor("#B3261E");

    private TextView status;
    /** v0.11.1：启动/停止合成的常驻按钮，文案随服务真实状态变化 */
    private Button recvToggleBtn;
    /** 启停是异步的：这段时间内不要用真实状态覆盖「正在启动/停止…」 */
    private volatile boolean recvPending;
    /** v0.12：设备与保活面板（发现、授权、信任、常驻、保活增强都收在这里） */
    private LinearLayout devicePanel;
    private TextView deviceToggle;
    /** v0.12：剪贴板面板 */
    private LinearLayout clipPanel;
    private TextView clipToggle;
    /** v0.12：底部两个展开页 */
    private TextView recentSendToggle;
    private TextView recentToggle;
    /** v0.12：强制重传开关 */
    private android.widget.CheckBox forceBox;
    private LinearLayout recent;
    /** 信任列表容器：每次刷新整体重建，列表很小，不值得做局部更新 */
    private LinearLayout trustList;

    // ---- 发送（M3b）----
    private LinearLayout peerList;
    private TextView sendTarget;
    private TextView sendPicked;
    private TextView sendProgressText;
    private android.widget.ProgressBar sendBar;
    /** v0.11：发送中的暂停/取消入口，只在发送期间显示 */
    private Button sendPauseBtn;
    private Button sendCancelBtn;
    private volatile boolean sendPaused;
    /** 用户点了取消：当前会话会被中断，后面排队的其它目标也不再开始 */
    private volatile boolean sendAborted;
    /** 已选中的绝对路径；一次发送里既可能是目录也可能是文件 */
    private final List<String> pickedPaths = new ArrayList<>();
    private String targetAddr = "";
    private String targetFp = "";
    private String targetName = "";
    /** 本机指纹：被对方拒绝时用户要拿它去批准，所以界面常驻显示，不必先失败一次才知道 */
    private String selfFp = "";
    private TextView selfFpText;
    private android.widget.EditText pairInput;
    /** v0.7：已选目标（addr / fp / name），支持一次选多台顺序发 */
    private final List<String[]> selectedPeers = new ArrayList<>();
    /** v0.16：对端可用落点（id / name）；多目标时一律用各自默认，不提供选择 */
    private final List<String[]> destOptions = new ArrayList<>();
    private String destId = "";
    private TextView destText;
    /** 界面当前显示的接收目录；从「所有文件访问」设置页回来时靠它判断落点是否已自动切换 */
    private String shownDest;
    /** 最近一次发现结果：勾选/取消后据此重画列表，不必再发一轮广播 */
    private String lastPeersJson;
    /** v0.7：最近发送列表容器 */
    private LinearLayout sendHistory;
    /** v0.8：剪贴板同步状态行 */
    private TextView clipStatus;
    /** v0.8：剪贴板端口（对端 clip_listen，与文件端口不同，默认 53320） */
    private android.widget.EditText clipPortInput;
    /** 剪贴板状态由后台服务更新，界面每 2 秒拉一次，避免显示停在「未开启」 */
    private final android.os.Handler uiTick = new android.os.Handler(android.os.Looper.getMainLooper());
    private final Runnable clipTick = new Runnable() {
        @Override
        public void run() {
            refreshClip();
            uiTick.postDelayed(this, 2000);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        int pad = dp(20);
        root.setPadding(pad, dp(16), pad, pad);
        // v0.13：浅灰底 + 白色卡片，层次靠留白与圆角，不靠多颜色
        root.setBackgroundColor(COLOR_BG);

        TextView title = new TextView(this);
        // v0.13.4：标题只留应用名，版本号挪到页面最后一行
        title.setText("局域网通传");
        title.setTextSize(24);
        title.setTypeface(android.graphics.Typeface.DEFAULT_BOLD);
        title.setTextColor(COLOR_INK);
        title.setGravity(Gravity.CENTER);
        title.setPadding(0, dp(8), 0, dp(20));
        root.addView(title);

        // v0.12：发现、授权、信任、常驻、保活增强全部收进一个展开面板。
        // v0.13：面板创建在这里、但等发送区之后再挂到 root，顺序按页面主次排。
        devicePanel = panel();
        deviceToggle = sectionToggle(DEVICE_TITLE, devicePanel, () -> {
            // 展开就拉一次真实状态：常驻/权限/信任都可能在面板收起期间变过
            refresh();
            refreshTrust();
        });

        // v0.11.1：按钮按功能分两行，同排两个各占一半宽度。
        // 「启动/停止」合成一个按钮：状态从服务真实状态推出来，避免两个按钮都亮着让人猜该点哪个。
        LinearLayout recvRow1 = new LinearLayout(this);
        recvRow1.setOrientation(LinearLayout.HORIZONTAL);
        recvToggleBtn = button("启动常驻接收", v -> {
            if (ReceiverService.isRunning()) {
                setWantRunning(false);
                recvPending = true;
                recvToggleBtn.setText("正在停止…");
                status.setText("常驻：正在停止…");
                status.setTextColor(COLOR_WARN);
                stopService(new android.content.Intent(this, ReceiverService.class));
                waitRecvState(false);
            } else {
                ensureNotificationPermission();
                setWantRunning(true);
                recvPending = true;
                // 启动是异步的：先给一句「正在启动」，再轮询真实状态，别让按钮看起来没反应
                recvToggleBtn.setText("正在启动…");
                status.setText("常驻：正在启动…（等待端口就绪）");
                status.setTextColor(COLOR_WARN);
                ReceiverService.start(this);
                waitRecvState(true);
            }
        });
        addWeighted(recvRow1, recvToggleBtn, true);
        // 刷新要连信任列表一起刷：被拒后用户点这里就该看到「待确认」，
        // 否则必须退出重进 Activity 才出现，等于把批准入口藏起来了
        Button refreshBtn = button("刷新状态", v -> {
            refresh();
        });
        addWeighted(recvRow1, refreshBtn, false);
        devicePanel.addView(recvRow1);

        LinearLayout recvRow2 = new LinearLayout(this);
        recvRow2.setOrientation(LinearLayout.HORIZONTAL);
        Button pickDestBtn = button("选择手机接收目录", v -> {
            if (!Dest.hasAllFilesAccess()) {
                android.widget.Toast.makeText(this,
                        "请先点「授予所有文件权限」，否则只能存到应用私有目录",
                        android.widget.Toast.LENGTH_LONG).show();
            }
            try {
                startActivityForResult(
                        new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE), REQ_TREE);
            } catch (Exception e) {
                android.widget.Toast.makeText(this, "打不开目录选择器：" + e.getMessage(),
                        android.widget.Toast.LENGTH_LONG).show();
            }
        });
        addWeighted(recvRow2, pickDestBtn, true);
        Button grantBtn = button("授予所有文件权限", v -> {
            try {
                Intent i = new Intent(
                        android.provider.Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION);
                i.setData(android.net.Uri.parse("package:" + getPackageName()));
                startActivity(i);
            } catch (Exception e) {
                android.widget.Toast.makeText(this, "打不开权限页：" + e.getMessage(),
                        android.widget.Toast.LENGTH_LONG).show();
            }
        });
        addWeighted(recvRow2, grantBtn, false);
        devicePanel.addView(recvRow2);

        // 选过自定义目录后再也回不到默认，是这次改默认落点后暴露出来的缺口：
        // 老用户存着旧的自定义路径，新默认对他就等于没生效，所以给一条明确的回程。
        devicePanel.addView(button("恢复默认接收目录（Download/" + Dest.DEFAULT_DIR_NAME + "）", v -> {
            Dest.clear(this);
            if (ReceiverService.isRunning()) {
                ReceiverService.restart(this);
            }
            android.widget.Toast.makeText(this,
                    "已恢复默认：" + Dest.displayPath(Dest.current(this)),
                    android.widget.Toast.LENGTH_LONG).show();
            recreate();
        }));

        // v0.20：出码本身属于「接收端动作」，放在常驻开关这一组里最顺手
        devicePanel.addView(button("配对二维码（让对方扫）", v -> showPairDialog()));

        // 保活两项：常驻自动开启后仍然保留手动入口，方便用户重新开启或去系统里关掉
        LinearLayout keepAliveBox = new LinearLayout(this);
        keepAliveBox.setOrientation(LinearLayout.HORIZONTAL);
        Button a11yBtn = button("增强后台（无障碍）", v -> {
            try {
                startActivity(new Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS));
            } catch (Exception e) {
                android.widget.Toast.makeText(this, "打不开无障碍设置：" + e.getMessage(),
                        android.widget.Toast.LENGTH_LONG).show();
            }
        });
        addWeighted(keepAliveBox, a11yBtn, true);
        Button batteryBtn = button("加入电池白名单", v -> {
            try {
                Intent i = new Intent(
                        android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS);
                i.setData(android.net.Uri.parse("package:" + getPackageName()));
                startActivity(i);
            } catch (Exception e) {
                startActivity(new Intent(
                        android.provider.Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS));
            }
        });
        addWeighted(keepAliveBox, batteryBtn, false);
        devicePanel.addView(keepAliveBox);

        // 发现与信任也收进面板：它们是「偶尔配置一次」的东西，不该占首页
        devicePanel.addView(button("发现附近的设备", v -> discoverPeersAsync()));
        peerList = new LinearLayout(this);
        peerList.setOrientation(LinearLayout.VERTICAL);
        devicePanel.addView(peerList);

        // ---- 发送区（v0.13：目标一行，操作收进一张卡片）----
        LinearLayout targetRow = new LinearLayout(this);
        targetRow.setOrientation(LinearLayout.HORIZONTAL);
        targetRow.setGravity(Gravity.CENTER_VERTICAL);
        targetRow.setPadding(dp(14), 0, dp(14), dp(10));
        TextView sendLabel = new TextView(this);
        sendLabel.setText("发送到");
        sendLabel.setTextSize(17);
        sendLabel.setTextColor(COLOR_MUTED);
        sendLabel.setLayoutParams(new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.WRAP_CONTENT, 0.34f));
        targetRow.addView(sendLabel);

        sendTarget = new TextView(this);
        sendTarget.setTextSize(14);
        sendTarget.setTextColor(COLOR_INK);
        sendTarget.setText("尚未选择目标");
        LinearLayout.LayoutParams targetLp = new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.WRAP_CONTENT, 0.66f);
        targetLp.setMargins(dp(10), 0, 0, 0);
        sendTarget.setLayoutParams(targetLp);
        // v0.13.4：点目标摘要直接进选设备——展开「设备与保活」并立刻发现一次
        sendTarget.setOnClickListener(v -> openDevicePanelAndDiscover());
        targetRow.addView(sendTarget);
        root.addView(targetRow);

        // v0.16：接收位置一行。选定目标后在后台查一次，默认就是对方的默认落点。
        LinearLayout destRow = new LinearLayout(this);
        destRow.setOrientation(LinearLayout.HORIZONTAL);
        destRow.setGravity(Gravity.CENTER_VERTICAL);
        destRow.setPadding(dp(14), 0, dp(14), dp(10));
        TextView destLabel = new TextView(this);
        destLabel.setText("接收位置");
        destLabel.setTextSize(17);
        destLabel.setTextColor(COLOR_MUTED);
        destLabel.setLayoutParams(new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.WRAP_CONTENT, 0.34f));
        destRow.addView(destLabel);
        destText = new TextView(this);
        destText.setTextSize(14);
        destText.setTextColor(COLOR_INK);
        destText.setText("默认");
        LinearLayout.LayoutParams destLp = new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.WRAP_CONTENT, 0.66f);
        destLp.setMargins(dp(10), 0, 0, 0);
        destText.setLayoutParams(destLp);
        destText.setOnClickListener(v -> chooseDestination());
        destRow.addView(destText);
        root.addView(destRow);

        LinearLayout sendCard = card();

        // v0.12：两个选择入口放同一行，各占一半
        LinearLayout pickRow = new LinearLayout(this);
        pickRow.setOrientation(LinearLayout.HORIZONTAL);
        Button pickFilesBtn = button("选择文件（可多选）", v -> pickSendFiles());
        Button pickFolderBtn = button("选择文件夹", v -> pickSendFolder());
        addWeighted(pickRow, pickFilesBtn, true);
        addWeighted(pickRow, pickFolderBtn, false);
        sendCard.addView(pickRow);

        // v0.20：扫码配对。对方手机出码，这边扫一次就填好目标与令牌，
        // 不用先等发现报文，也不会选错设备
        sendCard.addView(button("扫二维码配对", v -> openScanner()));

        // v0.12：强制重传——接收端有同内容文件时也重传一遍，
        // 目标位置的同名旧文件改名让位（只改名不删除）
        forceBox = new android.widget.CheckBox(this);
        forceBox.setText("强制重传（忽略对方已有文件）");
        forceBox.setTextSize(13);
        forceBox.setTextColor(COLOR_MUTED);
        forceBox.setPadding(0, dp(2), 0, dp(2));
        sendCard.addView(forceBox);

        sendPicked = new TextView(this);
        sendPicked.setTextSize(13);
        sendPicked.setTextColor(COLOR_MUTED);
        sendPicked.setPadding(0, dp(8), 0, dp(6));
        sendPicked.setText("尚未选择要发送的内容");
        sendCard.addView(sendPicked);

        sendBar = new android.widget.ProgressBar(this, null,
                android.R.attr.progressBarStyleHorizontal);
        sendBar.setMax(100);
        sendBar.setVisibility(ViewGroup.GONE);
        sendCard.addView(sendBar);

        sendProgressText = new TextView(this);
        sendProgressText.setTextSize(13);
        sendProgressText.setTextColor(COLOR_MUTED);
        sendCard.addView(sendProgressText);

        LinearLayout sendBtnRow = new LinearLayout(this);
        sendBtnRow.setOrientation(LinearLayout.HORIZONTAL);
        // 主操作只有一个，用实心主色；暂停/取消仍是普通按钮，避免同屏三个重点
        Button sendBtn = primaryButton("开始发送", v -> startSend());
        // 三个按钮平分一行：之前按内容宽度排，窄屏上「暂停/取消」会被挤出屏幕外
        sendPauseBtn = button("暂停", v -> togglePauseSend());
        sendCancelBtn = button("取消发送", v -> cancelSend());
        // 暂停/取消只在发送期间有意义；常驻显示会让人以为随时能点
        sendPauseBtn.setVisibility(ViewGroup.GONE);
        sendCancelBtn.setVisibility(ViewGroup.GONE);
        addWeighted(sendBtnRow, sendBtn, true);
        addWeighted(sendBtnRow, sendPauseBtn, false);
        addWeighted(sendBtnRow, sendCancelBtn, false);
        sendCard.addView(sendBtnRow);
        root.addView(sendCard);

        // v0.13.4：面板到这里才挂到页面上：顺序是「发送 → 设备与保活」
        root.addView(deviceToggle);
        root.addView(devicePanel);

        // v0.8：剪贴板同步。对端直接沿用上面的发送目标，少一次选择也少一处配错的地方
        clipPanel = panel();
        clipToggle = sectionToggle("剪贴板同步", clipPanel, this::refreshClip);
        root.addView(clipToggle);
        root.addView(clipPanel);

        TextView clipHint = new TextView(this);
        clipHint.setTextSize(12);
        clipHint.setText("对端与上面的发送目标相同。电脑复制 → 手机自动进剪贴板；"
                + "手机 → 电脑要在这里点一下（或通知/磁贴），系统不允许后台读剪贴板。");
        clipPanel.addView(clipHint);

        clipStatus = new TextView(this);
        clipStatus.setTextSize(13);
        clipStatus.setPadding(0, dp(6), 0, 0);
        clipPanel.addView(clipStatus);

        // 端口单独填：发现报文只带文件端口，剪贴板走的是对端 clip_listen 那个端口
        clipPortInput = new android.widget.EditText(this);
        clipPortInput.setHint("剪贴板端口（默认 53320）");
        clipPortInput.setInputType(InputType.TYPE_CLASS_NUMBER);
        clipPortInput.setTextSize(14);
        clipPortInput.setText(getSharedPreferences(Dest.PREFS, MODE_PRIVATE)
                .getString("clip_port", "53320"));
        clipPanel.addView(clipPortInput);

        clipPanel.addView(button("开启剪贴板同步", v -> {
            // 前台服务必须有通知权限，Android 13+ 没授权会直接把服务拒掉
            ensureNotificationPermission();
            saveClipPort();
            ClipService.start(this);
            refreshClip();
        }));
        clipPanel.addView(button("停止剪贴板同步", v -> {
            ClipService.stop(this);
            refreshClip();
        }));
        clipPanel.addView(button("把当前剪贴板推给电脑", v -> pushClipboardNow()));

        TextView path = new TextView(this);
        path.setTextSize(12);
        path.setPadding(0, dp(16), 0, 0);
        path.setText(receiveDirText());
        shownDest = Dest.current(this).getAbsolutePath();
        devicePanel.addView(path);

        // 「信任策略」从最上方状态区搬进面板：它属于配置，不该常驻首页
        TextView policy = new TextView(this);
        policy.setTextSize(12);
        policy.setPadding(0, dp(8), 0, 0);
        policy.setText("信任策略：未批准的电脑一律拒绝（在下方「信任的电脑」里批准）。");
        devicePanel.addView(policy);

        TextView trustTitle = new TextView(this);
        trustTitle.setText("信任的电脑");
        trustTitle.setTextSize(15);
        trustTitle.setPadding(0, dp(12), 0, dp(4));
        devicePanel.addView(trustTitle);

        trustList = new LinearLayout(this);
        trustList.setOrientation(LinearLayout.VERTICAL);
        devicePanel.addView(trustList);

        // v0.13.4：配对码与本机指纹并入「设备与保活」，放在面板末尾
        pairInput = new android.widget.EditText(this);
        // v0.20：扫码得到的是一次性令牌（10 位字母数字），不再是纯数字配对码，
        // 所以输入类型改成文本；手输 6 位静态码照样能用。
        pairInput.setHint("配对码 / 一次性令牌（扫码后自动填入）");
        pairInput.setInputType(InputType.TYPE_CLASS_TEXT);
        pairInput.setTextSize(14);
        pairInput.setPadding(0, dp(12), 0, dp(2));
        devicePanel.addView(pairInput);

        selfFpText = new TextView(this);
        selfFpText.setTextSize(12);
        selfFpText.setTextColor(COLOR_MUTED);
        selfFpText.setText("本机指纹：读取中…");
        devicePanel.addView(selfFpText);

        // v0.7：最近发送。以前一批发完只剩界面上那行摘要，关掉应用就查不到了
        // v0.12：和「最近接收」一起放到最下方，各是一个展开页
        sendHistory = panel();
        recentSendToggle = sectionToggle("最近发送", sendHistory, this::refreshSendHistory);
        root.addView(recentSendToggle);
        root.addView(sendHistory);
        sendHistory.setVisibility(ViewGroup.GONE);
        updateToggleText(recentSendToggle, "最近发送", false);

        // v0.12：最近接收也做成底部展开页
        recent = panel();
        recentToggle = sectionToggle("最近接收", recent, this::refreshRecent);
        root.addView(recentToggle);
        root.addView(recent);
        recent.setVisibility(ViewGroup.GONE);
        updateToggleText(recentToggle, "最近接收", false);

        // v0.13：运行状态放页面最下方——首页优先给发送操作，状态是「要看时再看」的信息
        status = new TextView(this);
        status.setTextSize(12);
        status.setTextColor(COLOR_MUTED);
        status.setLineSpacing(dp(3), 1f);
        status.setPadding(dp(4), dp(20), dp(4), dp(12));
        root.addView(status);

        // v0.13.4：版本号从标题挪到页面最后一行
        TextView verText = new TextView(this);
        verText.setText("版本 " + versionName());
        verText.setTextSize(11);
        verText.setTextColor(COLOR_MUTED);
        verText.setGravity(Gravity.CENTER);
        verText.setPadding(0, dp(2), 0, dp(10));
        root.addView(verText);

        // 内容会随接收记录变长，套一层滚动容器，避免小屏上按钮被挤出屏幕
        android.widget.ScrollView scroll = new android.widget.ScrollView(this);
        scroll.addView(root);
        // targetSdk 35 起系统强制边到边，内容会从 y=0 开始画、被状态栏和标题栏压住；
        // 让滚动容器自己吃掉系统窗口内边距，标题与状态前两行才不会被盖掉
        scroll.setFitsSystemWindows(true);
        setContentView(scroll);

        // v0.12：打开应用就自动常驻（含后台保活），用户不必再点「启动常驻接收」
        autoKeepAlive();
        // 首次进入也要读一次信任库：只靠 onResume 的话，界面刚建好时列表是空的
        refreshTrust();
        // 记住上次选过的发送目标，避免每次都要重新发现
        restoreSendTarget();
        // 从分享菜单进来的要先把文件收下，再让用户选目标
        handleShareIntent(getIntent());
        // 磁贴/通知点「推送剪贴板」：此时界面在前台，系统才允许读剪贴板
        if (getIntent() != null && getIntent().getBooleanExtra("clip_push", false)) {
            getIntent().removeExtra("clip_push");
            pushClipboardNow();
        }
        // M1 联调入口（无界面）：见 maybeRunProbe
        maybeRunProbe(getIntent());
    }

    /** 应用已在后台时被再次分享：复用当前界面，不叠新的 Activity（manifest 里 singleTop）。 */
    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        handleShareIntent(intent);
        // 磁贴/通知在应用还开着时也会走这里，必须和 onCreate 一样处理，否则点了没反应
        if (intent != null && intent.getBooleanExtra("clip_push", false)) {
            intent.removeExtra("clip_push");
            pushClipboardNow();
        }
    }

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
        // v0.16：探针可指定落点标识，便于自动化验证「选第二个位置」的真实落盘
        final String dest = intent.getStringExtra("tc_dest");
        final long timeoutMs = intent.getLongExtra("tc_timeout", 6000);
        final String identityDir = ReceiverService.identityDir(this).getAbsolutePath();
        // 发送会阻塞到会话结束，必须离开主线程，否则界面直接 ANR
        new Thread(() -> {
            String out;
            // v0.8 剪贴板探针：自动化验证走这条，避免占用用户界面与键鼠
            if ("clip_start".equals(cmd)) {
                ClipService.start(MainActivity.this);
                out = "{\"ok\":true,\"started\":true}";
            } else if ("clip_stop".equals(cmd)) {
                ClipService.stop(MainActivity.this);
                out = "{\"ok\":true,\"stopped\":true}";
            } else if ("clip_pull".equals(cmd)) {
                out = TcNative.clipPullJson(identityDir, ClipService.clipTarget(MainActivity.this),
                        pref("send_fp"), intent.getLongExtra("tc_since", 0), Build.MODEL);
            } else if ("clip_push".equals(cmd)) {
                String text = intent.getStringExtra("tc_text");
                out = TcNative.clipPushJson(identityDir, ClipService.clipTarget(MainActivity.this),
                        pref("send_fp"), text == null ? "" : text, false, Build.MODEL);
            } else if ("clip_read".equals(cmd)) {
                // 只回长度与是否命中期望文本，不把剪贴板内容写进日志。
                // 先等一下：系统要求应用真正拿到焦点后才放行读剪贴板，刚启动立刻读会得到空串。
                try {
                    Thread.sleep(intent.getLongExtra("tc_delay", 2000));
                } catch (InterruptedException ignored) {
                }
                String now = readClipboard();
                String expect = intent.getStringExtra("tc_expect");
                out = "{\"ok\":true,\"len\":" + now.length() + ",\"match\":"
                        + (expect != null && expect.equals(now)) + "}";
            } else if ("list".equals(cmd)) {
                out = TcNative.listPath(path == null ? "/storage/emulated/0" : path);
            } else if ("discover".equals(cmd)) {
                out = runDiscover(timeoutMs);
            } else if ("progress".equals(cmd)) {
                out = TcNative.sendProgress();
            } else if ("dests".equals(cmd)) {
                // v0.16：只查落点，不传文件；自动化用它确认手机能拿到 NAS 的清单
                out = TcNative.listDestinations(target, fp, "", identityDir, Build.MODEL);
            } else if ("send".equals(cmd)) {
                // 单个路径时用 JSONArray 现拼，避免在 adb 命令行里嵌套引号被 shell 吃掉
                String pj = paths;
                if (pj == null && path != null) {
                    pj = new JSONArray().put(path).toString();
                }
                // 设备名必须由宿主给：Android 没有 HOSTNAME，不传对端就显示 unknown
                // 另起线程轮询进度并打日志，用来验证「进度确实在递增」；正式界面改成刷新 UI
                Thread mon = new Thread(() -> {
                    long deadline = System.currentTimeMillis() + 3000;
                    boolean started = false;
                    while (true) {
                        String p = TcNative.sendProgress();
                        boolean active = p != null && p.contains("\"active\":true");
                        if (active) {
                            started = true;
                            android.util.Log.i("TONGCHUAN-PROBE", "PROGRESS " + p);
                        } else if (started) {
                            android.util.Log.i("TONGCHUAN-PROBE", "PROGRESS-DONE " + p);
                            return;
                        } else if (System.currentTimeMillis() > deadline) {
                            return;
                        }
                        try {
                            Thread.sleep(250);
                        } catch (InterruptedException e) {
                            return;
                        }
                    }
                }, "tc-progress");
                mon.start();
                // 探针也走正式那条路（前台服务）：自动化验证才能覆盖锁屏与通知
                SendService.start(MainActivity.this, target, fp, pj, "", identityDir, Build.MODEL,
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
                // 把界面会显示的那段「怎么了 + 下一步」也打进日志，便于自动化断言
                try {
                    JSONObject o = new JSONObject(out == null ? "{}" : out);
                    if (!o.optBoolean("ok")) {
                        android.util.Log.i("TONGCHUAN-PROBE",
                                "FRIENDLY " + friendlyFailure(o.optString("reason", "")));
                    }
                } catch (Exception ignored) {
                }
            } else {
                out = "{\"ok\":false,\"reason\":\"unknown tc_cmd\"}";
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
                getApplicationContext().getSystemService(android.content.Context.WIFI_SERVICE);
        android.net.wifi.WifiManager.MulticastLock lock = wm.createMulticastLock("tongchuan:discover");
        lock.setReferenceCounted(false);
        lock.acquire();
        try {
            return TcNative.discoverPeers(timeoutMs, ReceiverService.identityDir(this).getAbsolutePath());
        } finally {
            lock.release();
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        // 从「所有文件访问」设置页回来时，默认落点会自动从私有目录切到 Download/局域网通传。
        // 不在这里比对的话，界面还显示旧路径、接收端还往旧目录写，用户看到的就是「授权没生效」。
        String now = Dest.current(this).getAbsolutePath();
        if (shownDest != null && !shownDest.equals(now)) {
            shownDest = now;
            if (ReceiverService.isRunning()) {
                ReceiverService.restart(this);
            }
            recreate();
            return;
        }
        refresh();
        refreshTrust();
        refreshRecent();
        refreshSendHistory();
        refreshClip();
        uiTick.removeCallbacks(clipTick);
        uiTick.post(clipTick);
    }

    /**
     * 接收目录说明。
     * 新默认是「内置存储 / Download / 局域网通传」；没给「所有文件访问」时会退回应用私有目录，
     * 这里必须写清实际落点，否则用户会觉得显示的路径和预期对不上。
     */
    private String receiveDirText() {
        File cur = Dest.current(this);
        StringBuilder sb = new StringBuilder();
        sb.append("接收目录：\n").append(Dest.displayPath(cur))
                .append("\n（").append(cur.getAbsolutePath()).append("）");
        if (!Dest.hasAllFilesAccess()) {
            sb.append("\n未授予「所有文件访问」，暂存应用私有目录；")
                    .append("点上方「授予所有文件权限」后会自动切到 Download/")
                    .append(Dest.DEFAULT_DIR_NAME).append("。");
        }
        File old = Dest.legacyDir(this);
        // 旧默认目录里的文件既不迁移也不删除，只提示位置，免得用户以为收过的文件丢了
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

    @Override
    protected void onPause() {
        // 界面不在前台就别再刷了，省电也避免持有 Activity 引用
        uiTick.removeCallbacks(clipTick);
        super.onPause();
    }

    /**
     * 选完目录后把 SAF 树 URI 还原成真实路径存下来。
     * 选到网盘/U 盘这类没有稳定真实路径的位置时明确拒绝，而不是给个能选却写不进去的假选项。
     */
    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (resultCode != RESULT_OK || data == null) {
            return;
        }
        // ---- 发送：选文件（可多选）----
        if (requestCode == REQ_SEND_FILES) {
            pickedPaths.clear();
            List<Uri> uris = new ArrayList<>();
            android.content.ClipData clip = data.getClipData();
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
                android.widget.Toast.makeText(this,
                        "只支持手机内部存储里的文件夹；网盘/U 盘暂时发不了",
                        android.widget.Toast.LENGTH_LONG).show();
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
            android.widget.Toast.makeText(this,
                    "这个位置不是手机内置存储，暂时不支持；请选「内部存储」里的文件夹",
                    android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        Dest.save(this, path);
        android.widget.Toast.makeText(this, "接收目录已改为：" + path,
                android.widget.Toast.LENGTH_LONG).show();
        // 落点变了要换配置重开接收端，否则旧配置里的 destination 还指在旧目录
        if (ReceiverService.isRunning()) {
            ReceiverService.restart(this);
        }
        recreate();
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
        sendPicked.setText("正在读取已选的 " + uris.size() + " 项…");
        final List<Uri> snapshot = new ArrayList<>(uris);
        new Thread(() -> {
            List<String> resolved = new ArrayList<>();
            int skipped = 0;
            for (Uri u : snapshot) {
                String p = Dest.resolveAnyPath(MainActivity.this, u);
                if (p == null) {
                    skipped++;
                } else {
                    resolved.add(p);
                }
            }
            final int skip = skipped;
            runOnUiThread(() -> {
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
            sendPicked.setText(skipped > 0
                    ? "选中的 " + skipped + " 项都无法定位到手机存储，请改用「选择文件夹」或从「内部存储」里选"
                    : "尚未选择要发送的内容");
            return;
        }
        StringBuilder sb = new StringBuilder();
        // 选文件夹时只有一项，直接说「某个文件夹」；选文件时按父目录分组
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
        sendPicked.setText(sb.toString());
    }

    private void updateTargetLabel() {
        refreshDestinations();
        if (selectedPeers.isEmpty()) {
            sendTarget.setText("尚未选择目标");
            return;
        }
        // v0.13：首页只留「名称 + 地址」两行，指纹属于核对信息，设备面板里能看
        String[] first = selectedPeers.get(0);
        String firstName = first[2] == null || first[2].isEmpty() ? "未命名设备" : first[2];
        if (selectedPeers.size() == 1) {
            sendTarget.setText(firstName + "\n" + first[0]);
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
        sendTarget.setText("已选 " + selectedPeers.size() + " 个目标\n" + names);
    }

    /**
     * v0.16：选中单个目标后查一次可用落点。查询失败不影响发送——退回默认落点，
     * 用户仍能照常发；多目标时不提供选择，各用对方的默认位置。
     */
    private void refreshDestinations() {
        destOptions.clear();
        destId = "";
        if (destText == null) {
            return;
        }
        if (selectedPeers.size() != 1) {
            destText.setText(selectedPeers.isEmpty() ? "默认" : "各自默认（多目标）");
            return;
        }
        final String[] t = selectedPeers.get(0);
        final String pair = pairInput == null ? "" : pairInput.getText().toString().trim();
        final String identityDir = ReceiverService.identityDir(this).getAbsolutePath();
        destText.setText("查询中…");
        new Thread(() -> {
            String out = TcNative.listDestinations(t[0], t[1], pair, identityDir, Build.MODEL);
            runOnUiThread(() -> applyDestinations(t[0], out));
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
                    destOptions.add(new String[]{id, d.optString("name", id)});
                }
            }
        } catch (Exception ignored) {
        }
        destText.setText(destOptions.isEmpty() ? "默认" : "默认（点选，共 " + destOptions.size() + " 个可选）");
    }

    private void chooseDestination() {
        if (selectedPeers.size() != 1) {
            android.widget.Toast.makeText(this, "多目标时各用对方的默认位置",
                    android.widget.Toast.LENGTH_SHORT).show();
            return;
        }
        if (destOptions.isEmpty()) {
            // 可能是还没查、或对方只有默认位置：重查一次，查不到就说明没有别的可选
            refreshDestinations();
            return;
        }
        final List<String> labels = new ArrayList<>();
        labels.add("默认位置");
        int checked = 0;
        for (int i = 0; i < destOptions.size(); i++) {
            labels.add(destOptions.get(i)[1]);
            if (destOptions.get(i)[0].equals(destId)) {
                checked = i + 1;
            }
        }
        new android.app.AlertDialog.Builder(this)
                .setTitle("接收位置")
                .setSingleChoiceItems(labels.toArray(new String[0]), checked, (d, which) -> {
                    if (which == 0) {
                        destId = "";
                        destText.setText("默认");
                    } else {
                        destId = destOptions.get(which - 1)[0];
                        destText.setText(labels.get(which));
                    }
                    d.dismiss();
                })
                .setNegativeButton("取消", null)
                .show();
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
    private void togglePeer(String addr, String fp, String name) {
        for (int i = 0; i < selectedPeers.size(); i++) {
            if (selectedPeers.get(i)[0].equals(addr)) {
                selectedPeers.remove(i);
                updateTargetLabel();
                if (lastPeersJson != null) {
                    renderPeers(lastPeersJson);
                }
                return;
            }
        }
        selectedPeers.add(new String[]{addr, fp, name});
        // 记住最后一次选中的那台，下次进应用不用重新发现（多选不落盘，避免过期的目标组合）
        getSharedPreferences(Dest.PREFS, MODE_PRIVATE).edit()
                .putString("send_addr", addr)
                .putString("send_fp", fp)
                .putString("send_name", name)
                .apply();
        updateTargetLabel();
        if (lastPeersJson != null) {
            renderPeers(lastPeersJson);
        }
    }

    private static String shortFp(String fp) {
        return fp == null || fp.length() < 8 ? "—" : fp.substring(0, 8);
    }

    private void restoreSendTarget() {
        android.content.SharedPreferences p = getSharedPreferences(Dest.PREFS, MODE_PRIVATE);
        targetAddr = p.getString("send_addr", "");
        targetFp = p.getString("send_fp", "");
        targetName = p.getString("send_name", "");
        // 恢复出来的这台直接放进已选列表：上次能发，这次默认还是它
        if (!targetAddr.isEmpty() && !targetFp.isEmpty()) {
            selectedPeers.add(new String[]{targetAddr, targetFp, targetName});
        }
        updateTargetLabel();
    }

    private void discoverPeersAsync() {
        peerList.removeAllViews();
        TextView hint = new TextView(this);
        hint.setTextSize(13);
        hint.setText("正在发现…（约 5 秒）");
        peerList.addView(hint);
        new Thread(() -> {
            String json = runDiscover(5000);
            runOnUiThread(() -> renderPeers(json));
        }, "tc-discover").start();
    }

    private void renderPeers(String json) {
        lastPeersJson = json;
        peerList.removeAllViews();
        try {
            JSONObject o = new JSONObject(json == null ? "{}" : json);
            JSONArray arr = o.optJSONArray("peers");
            if (arr == null || arr.length() == 0) {
                TextView t = new TextView(this);
                t.setTextSize(13);
                t.setText("没有发现设备：确认在同一 Wi-Fi，且对端已开启接收");
                peerList.addView(t);
                return;
            }
            // 已选目标里那些这次没出现的（下线/换了地址）直接去掉：
            // 列表里没有它们就没法取消勾选，留着只会让每次发送都先失败一次
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
                final String addr = p.optString("addr");
                final String fp = p.optString("fingerprint");
                final String name = p.optString("name");
                String label = (isPeerSelected(addr) ? "✔ " : "")
                        + (name.isEmpty() ? "未命名" : name) + "  ·  " + addr
                        + "  ·  " + shortFp(fp);
                peerList.addView(button(label, v -> togglePeer(addr, fp, name)));
            }
        } catch (Exception e) {
            TextView t = new TextView(this);
            t.setTextSize(13);
            t.setText("发现结果解析失败：" + e.getMessage());
            peerList.addView(t);
        }
    }

    private void pickSendFiles() {
        try {
            Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            i.addCategory(Intent.CATEGORY_OPENABLE);
            i.setType("*/*");
            i.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            startActivityForResult(i, REQ_SEND_FILES);
        } catch (Exception e) {
            android.widget.Toast.makeText(this, "打不开文件选择器：" + e.getMessage(),
                    android.widget.Toast.LENGTH_LONG).show();
        }
    }

    private void pickSendFolder() {
        try {
            startActivityForResult(new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE), REQ_SEND_TREE);
        } catch (Exception e) {
            android.widget.Toast.makeText(this, "打不开目录选择器：" + e.getMessage(),
                    android.widget.Toast.LENGTH_LONG).show();
        }
    }

    // ---- v0.20 二维码配对：出码（接收端）与扫码（发送端）----

    private void openScanner() {
        try {
            startActivityForResult(new Intent(this, ScanActivity.class), REQ_SCAN);
        } catch (Exception e) {
            android.widget.Toast.makeText(this, "打不开扫码页：" + e.getMessage(),
                    android.widget.Toast.LENGTH_LONG).show();
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
            android.widget.Toast.makeText(this, "二维码解析失败",
                    android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        if (!o.optBoolean("ok")) {
            android.widget.Toast.makeText(this,
                    "这不是通传的配对二维码：" + o.optString("reason", "无法识别"),
                    android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        if (o.optBoolean("expired")) {
            android.widget.Toast.makeText(this, "二维码已过期，请让对方重新生成",
                    android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        String addr = o.optString("addr");
        String fp = o.optString("fp");
        String code = o.optString("code");
        String name = o.optString("name");
        // 令牌先填：紧随其后的落点查询会带它走同一场 TLS 会话，
        // 接收端据此直接把这台设备记进信任表
        if (pairInput != null) {
            pairInput.setText(code);
        }
        applyScannedPeer(addr, fp, name);
        String hint = o.optString("hint", "");
        String label = name.isEmpty() ? addr : name;
        android.widget.Toast.makeText(this,
                "已配对：" + label + (hint.isEmpty() ? "，选好文件即可发送" : "（" + hint + "）"),
                android.widget.Toast.LENGTH_LONG).show();
    }

    /** 扫码得到的目标直接覆盖已选列表并落盘：下次进应用默认还是它。 */
    private void applyScannedPeer(String addr, String fp, String name) {
        selectedPeers.clear();
        selectedPeers.add(new String[]{addr, fp, name});
        targetAddr = addr;
        targetFp = fp;
        targetName = name;
        getSharedPreferences(Dest.PREFS, MODE_PRIVATE).edit()
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
    private void showPairDialog() {
        // 出码意味着「我现在能接收」；常驻没开就顺手拉起，免得对方扫完却连不上
        if (!ReceiverService.isRunning()) {
            ensureNotificationPermission();
            setWantRunning(true);
            recvPending = true;
            recvToggleBtn.setText("正在启动…");
            status.setText("常驻：正在启动…（等待端口就绪）");
            status.setTextColor(COLOR_WARN);
            ReceiverService.start(this);
            waitRecvState(true);
        }
        final String dir = ReceiverService.identityDir(this).getAbsolutePath();
        final String name = "手机-" + Build.MODEL;
        final android.content.SharedPreferences prefs =
                getSharedPreferences(Dest.PREFS, MODE_PRIVATE);

        LinearLayout box = new LinearLayout(this);
        box.setOrientation(LinearLayout.VERTICAL);
        box.setPadding(dp(20), dp(12), dp(20), dp(4));

        TextView tip = new TextView(this);
        tip.setTextSize(13);
        tip.setTextColor(COLOR_MUTED);
        tip.setText("让对方打开通传 →「扫二维码配对」扫这张码，扫到后直接发文件。"
                + "有效期 10 分钟，只能配一台设备；关掉这个窗口不影响它继续可用。");
        box.addView(tip);

        final android.widget.ImageView qrView = new android.widget.ImageView(this);
        qrView.setAdjustViewBounds(true);
        LinearLayout.LayoutParams qlp = new LinearLayout.LayoutParams(dp(220), dp(220));
        qlp.gravity = Gravity.CENTER_HORIZONTAL;
        qlp.topMargin = dp(12);
        qrView.setLayoutParams(qlp);
        box.addView(qrView);

        final TextView info = new TextView(this);
        info.setTextSize(13);
        info.setTextColor(COLOR_ACCENT);
        info.setGravity(Gravity.CENTER);
        info.setPadding(0, dp(8), 0, dp(8));
        info.setText("准备中…");
        box.addView(info);

        final android.widget.EditText addrInput = new android.widget.EditText(this);
        addrInput.setHint("本机地址（自动判断，可改成对方能连到的那个）");
        addrInput.setTextSize(13);
        addrInput.setText(prefs.getString("pair_addr", ""));
        box.addView(addrInput);

        LinearLayout btnRow = new LinearLayout(this);
        btnRow.setOrientation(LinearLayout.HORIZONTAL);
        Button regenBtn = button("重新生成", null);
        Button revokeBtn = button("立即作废", null);
        addWeighted(btnRow, regenBtn, true);
        addWeighted(btnRow, revokeBtn, false);
        box.addView(btnRow);

        final android.app.AlertDialog dlg = new android.app.AlertDialog.Builder(this)
                .setTitle("配对二维码").setView(box).setPositiveButton("关闭", null).create();
        final android.os.Handler h = new android.os.Handler(android.os.Looper.getMainLooper());
        final long[] expires = {0};
        final Runnable[] tick = new Runnable[1];
        tick[0] = () -> {
            long left = Math.max(0, expires[0] - System.currentTimeMillis() / 1000);
            if (left <= 0) {
                info.setTextColor(COLOR_WARN);
                info.setText("已过期，请点「重新生成」");
                return;
            }
            info.setTextColor(COLOR_ACCENT);
            info.setText(String.format(Locale.CHINA, "剩余 %d:%02d · 一次有效",
                    left / 60, left % 60));
            h.postDelayed(tick[0], 1000);
        };
        // 出码 / 停码都要在后台线程做：JNI 里要读身份文件与系统随机数，别卡住界面
        final Runnable[] issue = new Runnable[1];
        issue[0] = () -> {
            final String addr = addrInput.getText().toString().trim();
            prefs.edit().putString("pair_addr", addr).apply();
            new Thread(() -> {
                final String json = TcNative.pairIssue(dir, addr, name);
                runOnUiThread(() -> {
                    if (renderPairJson(json, qrView, info, addrInput, expires)) {
                        h.removeCallbacks(tick[0]);
                        h.post(tick[0]);
                    }
                });
            }, "tc-pair").start();
        };
        regenBtn.setOnClickListener(v -> issue[0].run());
        revokeBtn.setOnClickListener(v -> new Thread(() -> {
            TcNative.pairRevoke(dir);
            runOnUiThread(() -> {
                h.removeCallbacks(tick[0]);
                expires[0] = 0;
                qrView.setImageDrawable(null);
                info.setTextColor(COLOR_WARN);
                info.setText("已作废：任何设备都不能再凭这张码配对");
            });
        }, "tc-pair").start());

        new Thread(() -> {
            final String saved = prefs.getString("pair_addr", "");
            String st = TcNative.pairStatus(dir, saved, name);
            boolean active = st != null && st.contains("\"active\":true");
            final String json = active ? st : TcNative.pairIssue(dir, saved, name);
            runOnUiThread(() -> {
                if (renderPairJson(json, qrView, info, addrInput, expires)) {
                    h.removeCallbacks(tick[0]);
                    h.post(tick[0]);
                }
            });
        }, "tc-pair").start();

        // 扫屏时别让屏幕自己黑掉：倒计时在走、对方正对着扫
        dlg.setOnShowListener(d -> {
            if (dlg.getWindow() != null) {
                dlg.getWindow().addFlags(
                        android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            }
        });
        // 关窗口只停止展示与倒计时，令牌留在接收端，10 分钟内仍可被扫
        dlg.setOnDismissListener(d -> h.removeCallbacks(tick[0]));
        dlg.show();
    }

    /** 把 Rust 返回的配对 JSON 画成二维码与倒计时状态。 */
    private boolean renderPairJson(String json, android.widget.ImageView qrView, TextView info,
                                   android.widget.EditText addrInput, long[] expires) {
        try {
            JSONObject o = new JSONObject(json == null ? "{}" : json);
            if (!o.optBoolean("ok") || !o.optBoolean("active")) {
                expires[0] = 0;
                qrView.setImageDrawable(null);
                info.setTextColor(COLOR_WARN);
                info.setText(o.optString("reason", "当前没有有效二维码"));
                return false;
            }
            String addr = o.optString("addr", "");
            if (!addr.isEmpty()) {
                addrInput.setText(addr);
            }
            android.graphics.Bitmap bmp = qrBitmap(o.optString("payload"), dp(220));
            if (bmp == null) {
                info.setTextColor(COLOR_ERR);
                info.setText("二维码生成失败");
                return false;
            }
            qrView.setImageBitmap(bmp);
            expires[0] = o.optLong("expires");
            return true;
        } catch (Exception e) {
            info.setTextColor(COLOR_ERR);
            info.setText("二维码数据解析失败");
            return false;
        }
    }

    /** 用 zxing 把配对文本画成位图。M 级纠错：屏幕直扫，容得下轻微反光与摩尔纹。 */
    private static android.graphics.Bitmap qrBitmap(String text, int size) {
        if (text == null || text.isEmpty() || size <= 0) {
            return null;
        }
        try {
            java.util.Map<com.google.zxing.EncodeHintType, Object> hints =
                    new java.util.EnumMap<>(com.google.zxing.EncodeHintType.class);
            hints.put(com.google.zxing.EncodeHintType.CHARACTER_SET, "UTF-8");
            hints.put(com.google.zxing.EncodeHintType.MARGIN, 1);
            hints.put(com.google.zxing.EncodeHintType.ERROR_CORRECTION,
                    com.google.zxing.qrcode.decoder.ErrorCorrectionLevel.M);
            com.google.zxing.common.BitMatrix m = new com.google.zxing.qrcode.QRCodeWriter()
                    .encode(text, com.google.zxing.BarcodeFormat.QR_CODE, size, size, hints);
            int[] px = new int[size * size];
            for (int y = 0; y < size; y++) {
                for (int x = 0; x < size; x++) {
                    px[y * size + x] = m.get(x, y) ? Color.BLACK : Color.WHITE;
                }
            }
            return android.graphics.Bitmap.createBitmap(
                    px, size, size, android.graphics.Bitmap.Config.ARGB_8888);
        } catch (Exception e) {
            return null;
        }
    }

    /** 发送：按已选目标逐个发，后台线程跑会话，另一个线程每 250ms 把进度刷到界面。 */
    private void startSend() {
        // 只记「有几台目标、选了几项」，不记任何路径或文件名
        android.util.Log.i("TONGCHUAN-SEND",
                "start targets=" + selectedPeers.size() + " picked=" + pickedPaths.size());
        if (selectedPeers.isEmpty()) {
            sendProgressText.setText("请先「发现附近的设备」并选一个目标");
            android.widget.Toast.makeText(this, "请先「发现附近的设备」并选一个目标",
                    android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        if (pickedPaths.isEmpty()) {
            sendProgressText.setText("请先选择文件或文件夹");
            android.widget.Toast.makeText(this, "请先选择文件或文件夹",
                    android.widget.Toast.LENGTH_LONG).show();
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
        final String pairCode = pairInput == null ? "" : pairInput.getText().toString().trim();
        // v0.12：强制重传开关，随清单一起告诉接收端「不要跳过已有文件」
        final boolean force = forceBox != null && forceBox.isChecked();
        final String identityDir = ReceiverService.identityDir(this).getAbsolutePath();
        if (SendService.isRunning()) {
            android.widget.Toast.makeText(this, "上一批还在发送，请等它结束",
                    android.widget.Toast.LENGTH_LONG).show();
            return;
        }
        // 前台服务必须有通知权限，否则 Android 13+ 直接拒绝启动
        ensureNotificationPermission();

        // 每一批都是新的控制状态：上一批的暂停/取消不能带过来
        sendPaused = false;
        sendAborted = false;
        SendService.paused = false;
        sendPauseBtn.setText("暂停");
        sendPauseBtn.setVisibility(ViewGroup.VISIBLE);
        sendCancelBtn.setVisibility(ViewGroup.VISIBLE);

        sendBar.setVisibility(ViewGroup.VISIBLE);
        sendBar.setIndeterminate(true);
        sendBar.setProgress(0);
        // 选文件夹时，进度的第一段是「扫描 + 计算每个文件的 BLAKE3」，文件多会很慢且没有百分比，
        // 所以先用不确定态提示，等第一个带总字节的进度到了再切成确定进度条
        sendProgressText.setText("正在扫描并计算校验值…（文件多时需要一会儿）");

        new Thread(() -> {
            final boolean[] running = {true};
            Thread poller = new Thread(() -> {
                // 每秒落一条网络采样：之前只有 start/done 两个时间点，只能算平均值，
                // 看不出 RSSI 与瞬时速率的关系；这条日志不再依赖 adb 在场
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
                            // 扫描阶段没有总字节：显示「已处理 N 个文件 / X」，避免看起来卡死
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
                        runOnUiThread(() -> {
                            if (known) {
                                sendBar.setIndeterminate(false);
                            }
                            sendBar.setProgress(pc);
                            sendProgressText.setText(tx);
                        });
                    }
                    // 不再「一见到 inactive 就退出」：多目标时两批之间必然有一段空闲
                    try {
                        Thread.sleep(250);
                    } catch (InterruptedException e) {
                        return;
                    }
                }
            }, "tc-send-progress");
            poller.start();

            // 真正的发送交给前台服务：界面退到后台或熄屏后，这里的线程随时可能被系统回收，
            // 而前台服务带通知与唤醒锁，能把「锁屏也发完」保住。
            // v0.7：多台目标顺序发——前台服务一次只跑一场，串行也是它本身的能力边界。
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
                runOnUiThread(() -> sendProgressText.setText(progress));

                // v0.16：落点只对单目标有意义；多目标时各用对方默认位置
                SendService.start(MainActivity.this, t[0], t[1], pathsJson, pairCode, identityDir,
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
                runOnUiThread(this::refreshSendHistory);
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
            runOnUiThread(() -> {
                // 小文件可能在「扫描」阶段就传完了，进度条从没切成确定态；
                // 收尾时必须显式停掉滚动并拉满，否则看起来像还在传
                sendBar.setIndeterminate(false);
                sendBar.setProgress(100);
                sendProgressText.setText(summary);
                sendPauseBtn.setVisibility(ViewGroup.GONE);
                sendCancelBtn.setVisibility(ViewGroup.GONE);
                sendPaused = false;
                SendService.paused = false;
                refreshSendHistory();
            });
        }, "tc-send").start();
    }

    /** 暂停 / 继续当前发送：状态只在块边界生效，点完立刻改按钮文案，不让用户以为没反应。 */
    private void togglePauseSend() {
        if (!SendService.isRunning()) {
            return;
        }
        sendPaused = !sendPaused;
        TcNative.sendPause(sendPaused);
        SendService.paused = sendPaused;
        sendPauseBtn.setText(sendPaused ? "继续" : "暂停");
        android.widget.Toast.makeText(this,
                sendPaused ? "已暂停，点「继续」接着传" : "已继续发送",
                android.widget.Toast.LENGTH_SHORT).show();
    }

    /** 取消当前发送：接收端会留下 .tcpart，下次同名发送可以续传。 */
    private void cancelSend() {
        if (!SendService.isRunning()) {
            return;
        }
        new android.app.AlertDialog.Builder(this)
                .setTitle("取消发送？")
                .setMessage("已传的部分会留在对方那边（下次同名发送可续传），未传的不会再发。")
                .setNegativeButton("继续发送", null)
                .setPositiveButton("取消发送", (d, w) -> {
                    sendAborted = true;
                    sendPaused = false;
                    SendService.paused = false;
                    TcNative.sendCancel();
                    sendProgressText.setText("正在取消…");
                    sendPauseBtn.setVisibility(ViewGroup.GONE);
                })
                .show();
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
            // v0.11：接收端回报落点文件夹，完成提示要能说清「传到哪了」
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
            // v0.12：完成提示分「新传 / 跳过」两个口径，用户能看出这次到底传了几个
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
            // 历史里留原始原因（英文），界面上再翻成中文；两者用途不同
            rec.reason = r.ok ? "" : reason;
            SendLog.append(this, rec);
        } catch (Exception e) {
            r.text = "发送结果解析失败：" + e.getMessage();
        }
        return r;
    }

    /** 最近发送：只显示最近 5 条，最新的在上；不显示文件路径与文件名。 */
    private void refreshSendHistory() {
        if (sendHistory == null) {
            return;
        }
        sendHistory.removeAllViews();
        List<SendLog.Record> list = SendLog.recent(this, 5);
        if (list.isEmpty()) {
            TextView t = new TextView(this);
            t.setTextSize(13);
            t.setText("还没有发送记录");
            sendHistory.addView(t);
            return;
        }
        for (SendLog.Record r : list) {
            TextView t = new TextView(this);
            t.setTextSize(13);
            String who = r.name.isEmpty() ? r.addr : r.name;
            // 历史里存原始原因，展示时翻成中文并只取第一句：列表里塞不下一整段「下一步」
            t.setText(timeLabel(r.ts) + " · " + who + " · "
                    + (r.ok ? "新传 " + Math.max(0, r.files - r.skipped) + " 个"
                    + (r.skipped > 0 ? " / 跳过 " + r.skipped + " 个" : "")
                    + " / " + human(r.bytes)
                    + (r.dest.isEmpty() ? "" : " → " + r.dest) + " · 成功"
                            : "失败：" + firstLine(friendlyFailure(r.reason))));
            sendHistory.addView(t);
        }
    }

    private static String firstLine(String s) {
        int i = s.indexOf('\n');
        return i < 0 ? s : s.substring(0, i);
    }

    // ---- v0.8 剪贴板同步 ----

    /** 读一项偏好；不存在时返回空串（剪贴板探针与推送共用）。 */
    private String pref(String key) {
        return getSharedPreferences(Dest.PREFS, MODE_PRIVATE).getString(key, "");
    }

    private void refreshClip() {
        if (clipStatus == null) {
            return;
        }
        String who = targetName == null || targetName.isEmpty() ? targetAddr : targetName;
        clipStatus.setText((ClipService.isRunning() ? "同步：已开启" : "同步：未开启")
                + " · 对端：" + (who == null || who.isEmpty() ? "未选择" : who)
                + "\n" + ClipService.lastStatus());
    }

    /** 端口留空就按默认 53320 存；关闭同步时不改用户填的值。 */
    private void saveClipPort() {
        if (clipPortInput == null) {
            return;
        }
        String p = clipPortInput.getText().toString().trim();
        if (p.isEmpty()) {
            p = "53320";
        }
        getSharedPreferences(Dest.PREFS, MODE_PRIVATE).edit().putString("clip_port", p).apply();
    }

    /**
     * 把当前剪贴板推给对端。
     * 只能在界面处于前台时调用：后台服务读剪贴板会被系统拦成空内容。
     */
    private void pushClipboardNow() {
        if (targetFp.isEmpty() || ClipService.clipTarget(this).isEmpty()) {
            toast("先在「发现附近的设备」里选一个目标");
            return;
        }
        saveClipPort();
        String text = readClipboard();
        if (text.isEmpty()) {
            toast("剪贴板是空的");
            return;
        }
        final String body = text;
        toast("正在推送…");
        new Thread(() -> {
            String out = TcNative.clipPushJson(
                    ReceiverService.identityDir(this).getAbsolutePath(),
                    ClipService.clipTarget(this), targetFp, body, false, Build.MODEL);
            String msg;
            try {
                JSONObject o = new JSONObject(out == null ? "{}" : out);
                msg = o.optBoolean("ok")
                        ? "已推送 " + o.optLong("size") + " 字节"
                        : "推送失败：" + o.optString("reason", "未知原因");
                // 日志只记成功与否和字节数，不记剪贴板内容
                android.util.Log.i("TONGCHUAN-CLIP", "push ok=" + o.optBoolean("ok")
                        + " size=" + o.optLong("size") + " bytes=" + body.length());
            } catch (Exception e) {
                msg = "推送结果解析失败：" + e.getMessage();
                android.util.Log.w("TONGCHUAN-CLIP", "push parse failed", e);
            }
            final String m = msg;
            runOnUiThread(() -> {
                toast(m);
                refreshClip();
            });
        }, "tc-clip-push").start();
    }

    /** 读系统剪贴板；取不到（后台/空/异常）一律返回空串，由调用方提示。 */
    private String readClipboard() {
        try {
            android.content.ClipboardManager cm = (android.content.ClipboardManager)
                    getSystemService(CLIPBOARD_SERVICE);
            if (cm == null || !cm.hasPrimaryClip() || cm.getPrimaryClip() == null) {
                return "";
            }
            ClipData cd = cm.getPrimaryClip();
            if (cd.getItemCount() == 0) {
                return "";
            }
            CharSequence t = cd.getItemAt(0).coerceToText(this);
            return t == null ? "" : t.toString();
        } catch (Exception e) {
            return "";
        }
    }

    /**
     * 把内核的英文原因翻成「怎么了 + 下一步」。
     * 用户不该为了看懂一次失败去翻 logcat，这也是 M4 的目的。
     */
    private String friendlyFailure(String reason) {
        String r = reason == null ? "" : reason;
        String lr = r.toLowerCase(Locale.getDefault());
        if (lr.contains("untrusted")) {
            return "对方还没有信任这台手机。\n"
                    + "下一步：去 NAS 管理页的「信任设备」里批准下面这串指纹；"
                    + "或在上面填对方设置的配对码后重试。\n"
                    + "本机指纹：" + fp8(selfFp);
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
        recent.removeAllViews();
        // v0.12：先把「最近一次接收」的结果放在最上面：来源、落点、跳过数，
        // 与发送侧和通知栏同一套口径，用户不用去翻通知历史
        String last = getSharedPreferences(Dest.PREFS, MODE_PRIVATE)
                .getString("last_recv", "");
        if (!last.isEmpty()) {
            String[] f = last.split("\\|", -1);
            TextView head = new TextView(this);
            head.setTextSize(13);
            head.setPadding(0, 0, 0, dp(8));
            if (f.length >= 7) {
                String device = f[0].isEmpty() ? "未知设备" : f[0];
                String roots = f[1].isEmpty() ? "接收目录" : f[1];
                long skipped = parseLong(f[2]);
                long files = parseLong(f[3]);
                long bytes = parseLong(f[4]);
                boolean interrupted = "1".equals(f[6]);
                head.setText("最近一次：" + device + " · 落点「" + roots + "」 · 新收 "
                        + Math.max(0, files - skipped) + " 个"
                        + (skipped > 0 ? " / 跳过 " + skipped + " 个" : "")
                        + " · " + human(bytes) + (interrupted ? " · 中断" : ""));
            }
            recent.addView(head);
        }
        List<File> files = new ArrayList<>();
        collect(Dest.current(this), files, 0);
        files.sort((a, b) -> Long.compare(b.lastModified(), a.lastModified()));

        if (files.isEmpty()) {
            TextView t = new TextView(this);
            t.setTextSize(13);
            t.setText("还没有收到文件");
            recent.addView(t);
            return;
        }
        SimpleDateFormat fmt = new SimpleDateFormat("MM-dd HH:mm", Locale.getDefault());
        int n = Math.min(20, files.size());
        for (int i = 0; i < n; i++) {
            File f = files.get(i);
            String label = f.getName() + "  ·  " + human(f.length()) + "  ·  "
                    + fmt.format(new Date(f.lastModified()));
            Button b = button(label, v -> openReceived(f));
            b.setTextSize(13);
            recent.addView(b);
        }
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

    private void openReceived(File f) {
        try {
            Uri uri = FileServeProvider.uriFor(this, f);
            Intent i = new Intent(Intent.ACTION_VIEW);
            i.setDataAndType(uri, getContentResolver().getType(uri));
            i.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            startActivity(i);
        } catch (Exception e) {
            android.widget.Toast.makeText(this, "没有能打开这个文件的应用",
                    android.widget.Toast.LENGTH_LONG).show();
        }
    }

    /**
     * 传一次 Wi-Fi 状态，把「信号强度 / 协商速率」和实测吞吐记进同一条日志。
     * BSSID 在 Android 12+ 没有定位权限时会被系统脱敏成 02:00:00:00:00:00，
     * 这不是 bug，照记即可——有值时正好能区分连的是光猫还是子路由。
     */
    private String wifiSample() {
        try {
            android.net.wifi.WifiManager wm = (android.net.wifi.WifiManager)
                    getApplicationContext().getSystemService(Context.WIFI_SERVICE);
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

    private String human(long n) {
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
        trustList.removeAllViews();
        String json = TcNative.trustJson(ReceiverService.identityDir(this).getAbsolutePath());
        if (json == null) {
            addTrustLine("信任列表读取失败", null, null);
            return;
        }
        try {
            JSONObject o = new JSONObject(json);
            // 本机指纹随信任快照一起返回：被对方拒绝时用户要拿它去批准，界面常驻显示更省事
            String mine = o.optString("selfFp", "");
            if (!mine.isEmpty()) {
                selfFp = mine;
                if (selfFpText != null) {
                    selfFpText.setText("本机指纹：" + fp8(mine) + "（对方批准时核对这串）");
                }
            }
            JSONArray pending = o.optJSONArray("pending");
            JSONArray peers = o.optJSONArray("peers");
            int p = pending == null ? 0 : pending.length();
            int k = peers == null ? 0 : peers.length();
            if (p == 0 && k == 0) {
                addTrustLine("还没有信任任何电脑。电脑第一次投递会被拒绝，批准后即可正常收。",
                        null, null);
                return;
            }
            for (int i = 0; i < p; i++) {
                JSONObject e = pending.getJSONObject(i);
                String label = "待确认：" + deviceLabel(e.optString("name"), e.optString("platform"))
                        + " · " + fp8(e.optString("fp")) + " · " + timeLabel(e.optLong("ts"));
                addTrustLine(label, "批准", () -> approve(e.optString("fp")));
            }
            for (int i = 0; i < k; i++) {
                JSONObject e = peers.getJSONObject(i);
                String alias = e.optString("alias");
                String label = "已信任：" + deviceLabel(alias.isEmpty() ? e.optString("name") : alias,
                        e.optString("platform")) + " · " + fp8(e.optString("fp"));
                addTrustLine(label, "撤销", () -> revoke(e.optString("fp")));
            }
        } catch (Exception e) {
            addTrustLine("信任列表解析失败：" + e.getMessage(), null, null);
        }
    }

    /** 一行「说明 + 可选按钮」；没有 action 时只显示说明文字。 */
    private void addTrustLine(String text, String actionLabel, Runnable action) {
        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setGravity(Gravity.CENTER_VERTICAL);

        TextView t = new TextView(this);
        t.setText(text);
        t.setTextSize(13);
        t.setLayoutParams(new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f));
        row.addView(t);

        if (action != null) {
            Button b = new Button(this);
            b.setText(actionLabel);
            b.setAllCaps(false);
            b.setInputType(InputType.TYPE_NULL);
            b.setTextSize(13);
            b.setOnClickListener(v -> action.run());
            row.addView(b);
        }
        trustList.addView(row);
    }

    private void approve(String fp) {
        boolean ok = TcNative.trustApprove(
                ReceiverService.identityDir(this).getAbsolutePath(), fp);
        toast(ok ? "已批准，这台电脑可以投递了" : "批准失败，请重试");
        refreshTrust();
    }

    private void revoke(String fp) {
        TcNative.trustRevoke(ReceiverService.identityDir(this).getAbsolutePath(), fp);
        toast("已撤销，这台电脑下次投递会被拒绝");
        refreshTrust();
    }

    /** 指纹只展示前 8 位：够用户区分设备，又不会把整屏占满。 */
    private String fp8(String fp) {
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
            return getPackageManager().getPackageInfo(getPackageName(), 0).versionName;
        } catch (Exception e) {
            return "";
        }
    }

    private void toast(String s) {
        android.widget.Toast.makeText(this, s, android.widget.Toast.LENGTH_SHORT).show();
    }

    private void refresh() {
        StringBuilder sb = new StringBuilder();
        sb.append(ReceiverService.isRunning() ? "常驻：运行中" : "常驻：未启动");
        String err = ReceiverService.lastError();
        if (err != null && !err.isEmpty()) {
            sb.append("\n").append(err);
        }
        sb.append("\n端口：53318（TCP + UDP）");
        // 信任策略已搬进「设备与保活」面板，首页只留一行运行状态
        sb.append("\n保活增强：").append(KeepAliveService.isEnabled(this) ? "已开启" : "未开启");
        sb.append("\n所有文件访问：").append(Dest.hasAllFilesAccess() ? "已授权" : "未授权（只能存应用私有目录）");
        status.setText(sb.toString());
        status.setTextColor(ReceiverService.isRunning() ? COLOR_OK : COLOR_ERR);
        // 合成按钮的文案跟着真实状态走；启停进行中保持「正在…」，别在两个状态之间闪
        if (recvToggleBtn != null && !recvPending) {
            recvToggleBtn.setText(ReceiverService.isRunning() ? "停止常驻接收" : "启动常驻接收");
        }
        // v0.17.1：状态刷新统一带上信任列表。之前只有「刷新状态」按钮单独补刷，
        // 服务启停、回到前台等入口只刷状态，被拒后的「待确认」要退出重进才看得到。
        if (devicePanel != null && devicePanel.getVisibility() == ViewGroup.VISIBLE) {
            refreshTrust();
        }
    }

    /**
     * v0.13.4：点「发送到」后面的目标摘要时，展开设备面板并立刻发现一次。
     * 点摘要的意图就是「换一台设备」，不该还要用户自己找展开入口、再点一次发现。
     */
    private void openDevicePanelAndDiscover() {
        if (devicePanel.getVisibility() != ViewGroup.VISIBLE) {
            devicePanel.setVisibility(ViewGroup.VISIBLE);
            updateToggleText(deviceToggle, DEVICE_TITLE, true);
        }
        refresh();
        refreshTrust();
        discoverPeersAsync();
    }

    /**
     * 折叠面板的标题。默认收起，点一下展开/收起。
     * 面板容器要先建好再传进来（标题只负责改它的可见性）。
     * onOpen 在展开时调用：列表与状态属于「点开就该是最新的」，不能等下一次 onResume。
     */
    private TextView sectionToggle(String title, LinearLayout panel, Runnable onOpen) {
        TextView t = new TextView(this);
        t.setTextSize(15);
        t.setTextColor(COLOR_ACCENT);
        t.setPadding(dp(4), dp(18), dp(4), dp(10));
        t.setOnClickListener(v -> {
            boolean open = panel.getVisibility() != ViewGroup.VISIBLE;
            panel.setVisibility(open ? ViewGroup.VISIBLE : ViewGroup.GONE);
            updateToggleText(t, title, open);
            if (open && onOpen != null) {
                onOpen.run();
            }
        });
        updateToggleText(t, title, panel.getVisibility() == ViewGroup.VISIBLE);
        return t;
    }

    /** 不需要展开时刷新数据的面板用这个重载。 */
    private TextView sectionToggle(String title, LinearLayout panel) {
        return sectionToggle(title, panel, null);
    }

    /** 展开/收起时同步箭头与提示语，用户一眼能看出现在是开还是关。 */
    private void updateToggleText(TextView t, String title, boolean open) {
        t.setText((open ? "▾ " : "▸ ") + title + (open ? "（点击收起）" : "（点这里展开）"));
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
            ReceiverService.start(this);
            // 不弹「已启动」的 Toast：每次开应用都弹一次太吵，状态行与按钮本身已经能看出来。
            // 服务是异步起来的，稍后再刷两次状态，避免底部一直显示「未启动」。
            uiTick.postDelayed(this::refresh, 800);
            uiTick.postDelayed(this::refresh, 2500);
        }
        android.content.SharedPreferences p = getSharedPreferences(Dest.PREFS, MODE_PRIVATE);
        if (p.getBoolean("auto_keepalive_done", false)) {
            return;
        }
        p.edit().putBoolean("auto_keepalive_done", true).apply();
        // 电池白名单：系统弹窗确认，应用不能替用户点
        try {
            Intent i = new Intent(
                    android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS);
            i.setData(android.net.Uri.parse("package:" + getPackageName()));
            startActivity(i);
        } catch (Exception ignored) {
            // 个别 ROM 没有这个页面：不影响接收，用户可在面板里手动开
        }
        // 无障碍必须用户亲手勾选；这里只把设置页打开一次，并说明为什么
        if (!KeepAliveService.isEnabled(this)) {
            toast("已自动开启常驻接收；后台保活请勾选「通传」无障碍服务（只需一次）");
            try {
                startActivity(new Intent(android.provider.Settings.ACTION_ACCESSIBILITY_SETTINGS));
            } catch (Exception ignored) {
            }
        }
    }

    /** 记「用户希望常驻」这个意图，开机自启只认这个开关，不认服务当前是否活着。 */
    private void setWantRunning(boolean want) {
        getSharedPreferences(BootReceiver.PREFS, MODE_PRIVATE)
                .edit().putBoolean(BootReceiver.KEY_WANT, want).apply();
    }

    /**
     * 启停按钮的即时反馈：先改状态文案，再每 300ms 看一眼真实状态，最多等 5 秒。
     * 之前点完只刷新一次，服务还没起来就显示「未启动」，看起来像按钮没生效。
     */
    private void waitRecvState(boolean want) {
        final long deadline = System.currentTimeMillis() + 5000;
        final Runnable[] tick = new Runnable[1];
        tick[0] = () -> {
            if (ReceiverService.isRunning() == want) {
                recvPending = false;
                refresh();
                android.widget.Toast.makeText(this,
                        want ? "常驻接收已启动" : "常驻接收已停止",
                        android.widget.Toast.LENGTH_SHORT).show();
                return;
            }
            if (System.currentTimeMillis() > deadline) {
                recvPending = false;
                refresh();
                android.widget.Toast.makeText(this,
                        "5 秒内没等到状态变化；点「刷新状态」再看，失败原因见上方提示",
                        android.widget.Toast.LENGTH_LONG).show();
                return;
            }
            uiTick.postDelayed(tick[0], 300);
        };
        uiTick.postDelayed(tick[0], 300);
    }

    private void ensureNotificationPermission() {
        if (Build.VERSION.SDK_INT >= 33
                && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, 1);
        }
    }

    private Button button(String text, android.view.View.OnClickListener click) {
        Button b = new Button(this);
        b.setText(text);
        b.setAllCaps(false);
        b.setInputType(InputType.TYPE_NULL);
        b.setOnClickListener(click);
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.bottomMargin = dp(8);
        b.setLayoutParams(lp);
        b.setGravity(Gravity.CENTER);
        return b;
    }

    /** 主操作按钮：实心主色 + 白字，全页只给「开始发送」用，保证一眼只有一个重点。 */
    private Button primaryButton(String text, android.view.View.OnClickListener click) {
        Button b = button(text, click);
        b.setTextColor(Color.WHITE);
        b.setTextSize(15);
        b.setBackground(rounded(COLOR_ACCENT, 10));
        return b;
    }

    /** 白色圆角卡片：发送区与各展开面板共用同一种容器，圆角只在卡片这一层出现。 */
    private LinearLayout card() {
        LinearLayout box = new LinearLayout(this);
        box.setOrientation(LinearLayout.VERTICAL);
        box.setBackground(rounded(COLOR_CARD, 12));
        box.setPadding(dp(14), dp(14), dp(14), dp(6));
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        lp.bottomMargin = dp(8);
        box.setLayoutParams(lp);
        return box;
    }

    /** 展开面板的容器：与发送卡片同一套材质，展开后就是一整块内容。 */
    private LinearLayout panel() {
        LinearLayout box = card();
        box.setVisibility(ViewGroup.GONE);
        return box;
    }

    /** 纯色圆角背景。代码构建界面没有 drawable 资源，统一在这里生成。 */
    private android.graphics.drawable.GradientDrawable rounded(int color, int radiusDp) {
        android.graphics.drawable.GradientDrawable g =
                new android.graphics.drawable.GradientDrawable();
        g.setColor(color);
        g.setCornerRadius(dp(radiusDp));
        return g;
    }

    /** 横向排两个按钮时各占一半：文案长短差别大，不设权重会把窄屏撑出屏幕。 */
    private void addWeighted(LinearLayout row, Button b, boolean first) {
        LinearLayout.LayoutParams lp = new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f);
        lp.bottomMargin = dp(8);
        if (!first) {
            lp.setMargins(dp(6), 0, 0, dp(8));
        }
        b.setLayoutParams(lp);
        row.addView(b);
    }

    private int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }
}
