package com.tongchuan.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.unit.dp
import com.tongchuan.TcController
import com.tongchuan.TrustItem
import kotlinx.coroutines.delay

/**
 * 设备页：常驻接收、接收目录、权限与保活、发现与信任、配对二维码。
 */
@Composable
fun DevicesScreen(controller: TcController) {
    val ui = controller.getUi()

    LazyColumn(
        modifier = Modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        // ---- 常驻 ----
        item {
            SectionCard(title = "常驻接收") {
                StatusCard(text = ui.statusText, ok = ui.statusOk)
                Button(
                    onClick = {
                        if (ui.recvRunning) controller.stopRecv() else controller.startRecv()
                    },
                    enabled = !ui.recvPending,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(
                        when {
                            ui.recvPending -> "正在切换…"
                            ui.recvRunning -> "停止常驻接收"
                            else -> "启动常驻接收"
                        }
                    )
                }
                // v0.17.1：被拒后的「待确认」只能靠刷新刷出来，必须有手动入口，
                // 否则批准入口又藏起来了（迁移时漏掉，这里补回）。
                OutlinedButton(
                    onClick = { controller.refreshAll() },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("刷新状态") }
            }
        }

        // ---- 接收目录 ----
        item {
            SectionCard(title = "接收目录") {
                Text(
                    text = ui.receiveDirText,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedButton(
                    onClick = { controller.openReceiveDirPicker() },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("选择手机接收目录") }
                OutlinedButton(
                    onClick = { controller.restoreDefaultRecvDir() },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("恢复默认接收目录（Download/局域网通传）") }
            }
        }

        // ---- 权限与保活 ----
        item {
            SectionCard(title = "权限与保活") {
                PermissionRow(
                    label = "所有文件访问",
                    state = if (ui.hasAllFilesAccess) "已授权" else "未授权",
                    action = "授予所有文件权限",
                    onClick = { controller.requestAllFiles() },
                )
                PermissionRow(
                    label = "无障碍增强",
                    state = if (ui.keepAliveEnabled) "已开启" else "未开启",
                    action = "打开无障碍设置",
                    onClick = { controller.openAccessibility() },
                )
                PermissionRow(
                    label = "电池白名单",
                    state = "",
                    action = "加入电池白名单",
                    onClick = { controller.openBatteryWhiteList() },
                )
            }
        }

        // ---- 发现与信任 ----
        item {
            SectionCard(title = "发现与信任") {
                OutlinedButton(
                    onClick = { controller.discoverPeersAsync() },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("发现附近的设备") }
                Text(
                    text = "信任策略：未批准的电脑一律拒绝，批准后即可正常收。",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                ui.trust.forEach { TrustRow(it, controller) }
            }
        }

        // ---- 配对 ----
        item {
            SectionCard(title = "配对") {
                OutlinedTextField(
                    value = ui.pairCode,
                    onValueChange = { controller.setPairCode(it) },
                    label = { Text("配对码 / 一次性令牌") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Text(
                    text = ui.selfFpText,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedButton(
                    onClick = { controller.showPairDialog() },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("配对二维码（让对方扫）") }
            }
        }
    }

    if (ui.pairDialogVisible) {
        PairQrDialog(controller)
    }
}

@Composable
private fun PermissionRow(label: String, state: String, action: String, onClick: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(label, style = MaterialTheme.typography.bodyMedium)
            if (state.isNotEmpty()) {
                Text(
                    state,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        OutlinedButton(onClick = onClick) { Text(action) }
    }
}

@Composable
private fun TrustRow(item: TrustItem, controller: TcController) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = item.label,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.weight(1f),
        )
        if (item.fp.isNotEmpty()) {
            OutlinedButton(onClick = {
                if (item.pending) controller.approve(item.fp) else controller.revoke(item.fp)
            }) {
                Text(if (item.pending) "批准" else "撤销")
            }
        }
    }
}

/**
 * 配对二维码对话框：状态来自控制器（二维码位图 + 过期时间戳），
 * 倒计时在这里每秒读一次时间戳渲染。
 */
@Composable
private fun PairQrDialog(controller: TcController) {
    val ui = controller.getUi()
    var now by remember { mutableLongStateOf(System.currentTimeMillis() / 1000) }
    LaunchedEffect(Unit) {
        while (true) {
            now = System.currentTimeMillis() / 1000
            delay(1000)
        }
    }
    val context = androidx.compose.ui.platform.LocalContext.current
    androidx.compose.runtime.DisposableEffect(Unit) {
        val window = (context as? android.app.Activity)?.window
        if (ui.pairKeepScreenOn) {
            window?.addFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
        onDispose {
            // 对方正举着手机扫，别让屏幕黑掉；关窗立即清除，避免常亮耗电
            window?.clearFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        }
    }
    val left = (ui.pairExpiresAt - now).coerceAtLeast(0)

    AlertDialog(
        onDismissRequest = { controller.dismissPairDialog() },
        title = { Text("配对二维码") },
        text = {
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(
                    "让对方打开通传 →「扫二维码配对」扫这张码，扫到后直接发文件。"
                            + "有效期 10 分钟，只能配一台设备；关掉窗口不影响它继续可用。",
                    style = MaterialTheme.typography.bodySmall,
                )
                val qr = ui.pairQr
                if (qr != null) {
                    Image(
                        bitmap = qr.asImageBitmap(),
                        contentDescription = "配对二维码",
                        modifier = Modifier.size(220.dp),
                    )
                }
                Text(
                    text = when {
                        ui.pairInfo.isNotEmpty() -> ui.pairInfo
                        left > 0 -> String.format(
                            java.util.Locale.CHINA, "剩余 %d:%02d · 一次有效", left / 60, left % 60)
                        else -> "已过期，请点「重新生成」"
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = if (left > 0) MaterialTheme.colorScheme.primary
                    else com.tongchuan.ui.theme.LocalExtendedColors.current.warning,
                )
                OutlinedTextField(
                    value = ui.pairAddr,
                    onValueChange = { controller.savePairAddr(it) },
                    label = { Text("本机地址（自动判断，可改）") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            TextButton(onClick = { controller.regenPairCode() }) { Text("重新生成") }
        },
        dismissButton = {
            TextButton(onClick = { controller.revokePairCode() }) { Text("立即作废") }
        },
    )
}
