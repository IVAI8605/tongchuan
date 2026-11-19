package com.tongchuan.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.tongchuan.DestOption
import com.tongchuan.HistoryItem
import com.tongchuan.PeerEntry
import com.tongchuan.TcController

/**
 * 发送页：选目标 → 选落点 → 选内容 → 发送 → 看进度与历史。
 * 所有状态都来自 {@link com.tongchuan.UiState}，用户动作回调控制器。
 */
@Composable
fun SendScreen(controller: TcController) {
    val ui = controller.getUi()

    LazyColumn(
        modifier = Modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        // ---- 目标 ----
        item {
            SectionCard(title = "发送到") {
                Text(
                    text = ui.sendTargetText,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 48.dp)
                        .clickable { controller.discoverPeersAsync() },
                )
                Text(
                    text = "点上面一行换目标；下方列表可多选",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        item {
            PeerList(
                peers = ui.peers,
                onToggle = { controller.togglePeer(it.addr, it.fp, it.name) },
            )
        }

        // ---- 落点 ----
        item {
            SectionCard(title = "接收位置") {
                Text(
                    text = ui.destText,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 48.dp)
                        .clickable { controller.chooseDestination() },
                )
            }
        }

        // ---- 选内容 ----
        item {
            SectionCard(title = "发送内容") {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(onClick = { controller.pickSendFiles() },
                        modifier = Modifier.weight(1f)) { Text("选择文件") }
                    OutlinedButton(onClick = { controller.pickSendFolder() },
                        modifier = Modifier.weight(1f)) { Text("选择文件夹") }
                }
                OutlinedButton(
                    onClick = { controller.openScanner() },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("扫二维码配对") }
                Text(
                    text = ui.pickedText,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(
                        checked = ui.force,
                        onCheckedChange = { controller.setForce(it) },
                    )
                    Text("强制重传（不跳过对方已有的同名文件）",
                        style = MaterialTheme.typography.bodySmall)
                }
            }
        }

        // ---- 发送 ----
        item {
            SectionCard(title = "开始发送") {
                Button(
                    onClick = { controller.startSend() },
                    enabled = !ui.sendRunning,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text(if (ui.sendRunning) "正在发送…" else "开始发送") }

                if (ui.sendRunning) {
                    if (ui.sendIndeterminate) {
                        LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                    } else {
                        LinearProgressIndicator(
                            progress = { ui.sendProgress / 100f },
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                    Text(
                        text = ui.sendProgressText,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        // 确认取消后到会话真正结束之间不能再暂停：
                        // 原 View 版确认取消时会把暂停按钮 GONE，这里等价隐藏。
                        if (!ui.sendCancelling) {
                            OutlinedButton(onClick = { controller.togglePauseSend() },
                                modifier = Modifier.weight(1f)) {
                                Text(if (ui.sendPaused) "继续" else "暂停")
                            }
                        }
                        OutlinedButton(onClick = { controller.cancelSend() },
                            modifier = Modifier.weight(1f)) { Text("取消") }
                    }
                } else if (ui.sendProgressText.isNotEmpty()) {
                    Text(
                        text = ui.sendProgressText,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }

        // ---- 历史 ----
        item {
            SectionCard(title = "最近发送") {
                if (ui.history.isEmpty()) {
                    Text("还没有发送记录",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant)
                } else {
                    ui.history.forEach { HistoryRow(it) }
                }
            }
        }
    }

    if (ui.destDialogVisible) {
        DestinationDialog(
            options = ui.destOptions,
            currentId = ui.destId,
            onPick = { controller.applyDestination(it) },
            onDismiss = { controller.dismissDestDialog() },
        )
    }

    if (ui.cancelConfirmVisible) {
        AlertDialog(
            onDismissRequest = { controller.dismissCancelDialog() },
            title = { Text("取消发送？") },
            text = { Text("已传的部分会留在对方那边（下次同名发送可续传），未传的不会再发。") },
            confirmButton = {
                TextButton(onClick = { controller.confirmCancelSend() }) { Text("取消发送") }
            },
            dismissButton = {
                TextButton(onClick = { controller.dismissCancelDialog() }) { Text("继续发送") }
            },
        )
    }
}

@Composable
private fun PeerList(peers: List<PeerEntry>, onToggle: (PeerEntry) -> Unit) {
    SectionCard(title = "附近设备") {
        if (peers.isEmpty()) {
            Text("点「发送到」那一行开始发现",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant)
            return@SectionCard
        }
        peers.forEach { peer ->
            if (peer.addr.isEmpty()) {
                // 发现中 / 空结果 / 解析失败：只展示文字，不可点
                ListItem(headlineContent = { Text(peer.name) })
            } else {
                ListItem(
                    headlineContent = { Text(peer.name) },
                    leadingContent = {
                        Checkbox(checked = peer.selected, onCheckedChange = { onToggle(peer) })
                    },
                    modifier = Modifier.clickable { onToggle(peer) },
                )
            }
        }
    }
}

@Composable
private fun HistoryRow(item: HistoryItem) {
    val ext = com.tongchuan.ui.theme.LocalExtendedColors.current
    Text(
        text = item.text,
        style = MaterialTheme.typography.bodySmall,
        color = if (item.ok) MaterialTheme.colorScheme.onSurfaceVariant
        else MaterialTheme.colorScheme.error,
    )
}

@Composable
private fun DestinationDialog(
    options: List<DestOption>,
    currentId: String,
    onPick: (Int) -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("接收位置") },
        text = {
            Column {
                TextButton(onClick = { onPick(0) }, modifier = Modifier.fillMaxWidth()) {
                    Text(if (currentId.isEmpty()) "● 默认位置" else "默认位置")
                }
                options.forEachIndexed { index, opt ->
                    TextButton(
                        onClick = { onPick(index + 1) },
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(if (opt.id == currentId) "● ${opt.name}" else opt.name)
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) { Text("取消") }
        },
    )
}
