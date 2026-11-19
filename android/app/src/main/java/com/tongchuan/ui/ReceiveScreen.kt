package com.tongchuan.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.tongchuan.RecentItem
import com.tongchuan.TcController

/**
 * 接收页：最近一次接收摘要 + 最近文件列表。
 * 列表来自控制器扫描接收目录（最多 20 条，最多 3 层）。
 */
@Composable
fun ReceiveScreen(controller: TcController) {
    val ui = controller.getUi()

    LazyColumn(
        modifier = Modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        if (ui.lastRecvText.isNotEmpty()) {
            item {
                SectionCard(title = "最近一次接收") {
                    Text(ui.lastRecvText, style = MaterialTheme.typography.bodySmall)
                }
            }
        }
        item {
            SectionCard(title = "最近接收") {
                if (ui.recent.isEmpty()) {
                    Text(
                        "还没有收到文件",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                } else {
                    Column {
                        ui.recent.forEach { recent ->
                            RecentRow(recent) { controller.openReceived(recent.file) }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun RecentRow(item: RecentItem, onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(item.name, style = MaterialTheme.typography.bodyMedium) },
        supportingContent = {
            Text("${item.sizeText}  ·  ${item.timeText}",
                style = MaterialTheme.typography.bodySmall)
        },
        modifier = Modifier.fillMaxWidth().clickable { onClick() },
    )
}
