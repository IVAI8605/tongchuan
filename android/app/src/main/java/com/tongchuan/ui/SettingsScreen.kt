package com.tongchuan.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.tongchuan.TcController
import com.tongchuan.TrustItem

/**
 * 设置页（v1.2.1）：接收目录 → 常驻接收 → 权限与保活 → 安全（默认关）。
 * v1.2.1 调整：接收目录放最上面、「安全」放最下面；安全区删掉多余的「展示二维码」按钮
 * （展示二维码改由发送页右上角扫码图标提供）。
 */
@Composable
fun SettingsScreen(controller: TcController) {
    val ui = controller.getUi()

    LazyColumn(
        modifier = Modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        // ---- 接收目录（最上）----
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

        // ---- 常驻接收 ----
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
                OutlinedButton(
                    onClick = { controller.refreshAll() },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("刷新状态") }
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

        // ---- 安全（最下，默认关）----
        item {
            SectionCard(title = "安全") {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text("启用安全功能", style = MaterialTheme.typography.bodyMedium)
                    }
                    Switch(
                        checked = ui.securityEnabled,
                        onCheckedChange = { controller.setSecurityEnabled(it) },
                    )
                }
                if (!ui.securityEnabled) {
                    Text(
                        "安全功能默认关闭。开启后可管理「发现与信任」与「配对」；关闭只是收起这些管理入口，" +
                                "不影响已建立连接的文件接收。",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } else {
                    Text("发现与信任", style = MaterialTheme.typography.titleSmall)
                    OutlinedButton(
                        onClick = { controller.discoverPeersAsync() },
                        modifier = Modifier.fillMaxWidth(),
                    ) { Text("发现附近的设备") }
                    Text(
                        "信任策略：未批准的电脑一律拒绝，批准后即可正常收。",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    ui.trust.forEach { TrustRow(it, controller) }

                    Text("配对", style = MaterialTheme.typography.titleSmall)
                    OutlinedTextField(
                        value = ui.pairCode,
                        onValueChange = { controller.setPairCode(it) },
                        label = { Text("配对码 / 一次性令牌") },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Text(
                        ui.selfFpText,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    // v1.2.1：这里不再放「展示二维码」按钮，改由发送页右上角扫码图标提供
                }
            }
        }
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
