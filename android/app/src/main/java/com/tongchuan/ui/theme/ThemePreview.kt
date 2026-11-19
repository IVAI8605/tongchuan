package com.tongchuan.ui.theme

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.Card
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp

/**
 * 主题令牌核对页：一张卡片 + 一组按钮 + 状态色 + 导航条。
 * 目的不是当页面用，而是让改色只需要看这一处预览。
 */
@Preview(showBackground = true, name = "主题令牌（浅色）")
@Composable
fun ThemePreviewLight() {
    TongchuanTheme(darkTheme = false) { ThemePreviewContent() }
}

@Preview(showBackground = true, name = "主题令牌（深色）")
@Composable
fun ThemePreviewDark() {
    TongchuanTheme(darkTheme = true) { ThemePreviewContent() }
}

@Composable
private fun ThemePreviewContent() {
    val ext = LocalExtendedColors.current
    Surface(color = MaterialTheme.colorScheme.background) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("局域网通传", style = MaterialTheme.typography.headlineSmall)
            Card {
                Column(modifier = Modifier.padding(16.dp)) {
                    Text("发送完成：新传 3 个，12.4 MB", style = MaterialTheme.typography.bodyMedium)
                    Text("落点：NAS 的「接收」文件夹", style = MaterialTheme.typography.bodySmall)
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = {}) { Text("开始发送") }
                OutlinedButton(onClick = {}) { Text("暂停") }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                StatusDot("成功", ext.success)
                StatusDot("警告", ext.warning)
                StatusDot("错误", MaterialTheme.colorScheme.error)
            }
            NavigationBar {
                NavigationBarItem(selected = true, onClick = {}, icon = { Text("发") }, label = { Text("发送") })
                NavigationBarItem(selected = false, onClick = {}, icon = { Text("设") }, label = { Text("设备") })
                NavigationBarItem(selected = false, onClick = {}, icon = { Text("剪") }, label = { Text("剪贴板") })
                NavigationBarItem(selected = false, onClick = {}, icon = { Text("收") }, label = { Text("接收") })
            }
        }
    }
}

@Composable
private fun StatusDot(label: String, color: androidx.compose.ui.graphics.Color) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(0.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        androidx.compose.foundation.layout.Box(
            modifier = Modifier.size(12.dp).background(color, CircleShape),
        )
        Text(label, style = MaterialTheme.typography.labelLarge)
    }
}
