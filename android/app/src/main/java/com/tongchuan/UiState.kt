package com.tongchuan

import android.graphics.Bitmap
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.io.File

/** 附近发现到的一台设备。 */
data class PeerEntry(
    val addr: String,
    val fp: String,
    val name: String,
    val selected: Boolean,
)

/** 对端可用的接收落点。 */
data class DestOption(val id: String, val name: String)

/** 一条发送历史（文案已按中文口径拼好）。 */
data class HistoryItem(val ts: Long, val who: String, val ok: Boolean, val text: String)

/** 接收目录里的一个文件，展示字段已经格式化好（Compose 侧不再做单位换算）。 */
data class RecentItem(
    val file: File,
    val name: String,
    val sizeText: String,
    val timeText: String,
)

/** 信任列表一行：pending=true 表示待确认。 */
data class TrustItem(val pending: Boolean, val label: String, val fp: String)

/**
 * 2026-11-19 UI 重构的状态桥：Java 控制器 {@link TcController} 写，Compose 读。
 *
 * 用 Compose 快照状态（mutableStateOf）而不是 StateFlow：Java 侧就是普通 setter，
 * 不需要collect；写入统一在主线程（控制器里用 runOnUiThread 包好）。
 * 列表一律整体替换，不做增量 diff。
 */
class UiState {
    // 顶部状态
    var statusText by mutableStateOf("")
    var statusOk by mutableStateOf(false)
    var recvRunning by mutableStateOf(false)
    var recvPending by mutableStateOf(false)

    // 发送页
    var peers by mutableStateOf(listOf<PeerEntry>())
    var sendTargetText by mutableStateOf("尚未选择目标")
    var destText by mutableStateOf("默认")
    var destOptions by mutableStateOf(listOf<DestOption>())
    var destId by mutableStateOf("")
    var destDialogVisible by mutableStateOf(false)
    var pickedText by mutableStateOf("尚未选择要发送的内容")
    var sendProgress by mutableStateOf(0)
    var sendIndeterminate by mutableStateOf(true)
    var sendProgressText by mutableStateOf("")
    var sendRunning by mutableStateOf(false)
    var sendPaused by mutableStateOf(false)
    /** 用户已确认取消、会话尚未真正结束：此时暂停按钮不能再点（原 View 版会直接 GONE）。 */
    var sendCancelling by mutableStateOf(false)
    var cancelConfirmVisible by mutableStateOf(false)
    var history by mutableStateOf(listOf<HistoryItem>())

    // 设备页
    var receiveDirText by mutableStateOf("")
    var hasAllFilesAccess by mutableStateOf(false)
    var keepAliveEnabled by mutableStateOf(false)
    var trust by mutableStateOf(listOf<TrustItem>())
    var selfFpText by mutableStateOf("本机指纹：读取中…")
    var pairCode by mutableStateOf("")

    // 配对二维码对话框
    var pairDialogVisible by mutableStateOf(false)
    /** 对话框展示期间保持屏幕常亮（对方正举着扫），关闭即清除。 */
    var pairKeepScreenOn by mutableStateOf(false)
    var pairQr by mutableStateOf<Bitmap?>(null)
    var pairInfo by mutableStateOf("")
    var pairExpiresAt by mutableStateOf(0L)
    var pairAddr by mutableStateOf("")

    // 剪贴板页

    // 接收页
    var recent by mutableStateOf(listOf<RecentItem>())
    var lastRecvText by mutableStateOf("")
    // v1.1.0：接收实时进度与速度（接收中才显示）
    var recvReceiving by mutableStateOf(false)
    var recvProgress by mutableStateOf(0)          // 0-100
    var recvSpeedText by mutableStateOf("")        // 形如 "87.3 MB/s"
    var recvProgressText by mutableStateOf("")     // 形如 "已收 512 MB / 1.2 GB"

    // 其他
    var force by mutableStateOf(false)
    /** v1.2.0：设置页「安全」总开关，默认关；开启后才显示发现与信任/配对的管理入口。 */
    var securityEnabled by mutableStateOf(false)
    var versionName by mutableStateOf("")
}
