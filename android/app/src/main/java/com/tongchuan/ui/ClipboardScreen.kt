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
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.unit.dp
import com.tongchuan.TcController

/**
 * 剪贴板页：同步开关、端口、立即推送。
 * 对端沿用发送页选中的目标，不重复选择。
 */
@Composable
fun ClipboardScreen(controller: TcController) {
    val ui = controller.getUi()

    LazyColumn(
        modifier = Modifier.fillMaxWidth(),
        contentPadding = androidx.compose.foundation.layout.PaddingValues(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item {
            SectionCard(title = "剪贴板同步") {
                Text(
                    text = ui.clipText,
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(
                    text = "电脑复制 → 手机自动进剪贴板；手机 → 电脑要在这里点一下"
                            + "（或通知/磁贴），系统不允许后台读剪贴板。",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        item {
            SectionCard(title = "端口") {
                Row(
                    verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    OutlinedTextField(
                        value = ui.clipPort,
                        onValueChange = { controller.setClipPort(it) },
                        label = { Text("剪贴板端口（默认 53320）") },
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                        modifier = Modifier.weight(1f),
                    )
                    OutlinedButton(onClick = { controller.saveClipPort() }) { Text("保存") }
                }
            }
        }
        item {
            SectionCard(title = "操作") {
                Button(
                    onClick = { controller.startClip() },
                    enabled = true,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("开启剪贴板同步") }
                OutlinedButton(
                    onClick = { controller.stopClip() },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("停止剪贴板同步") }
                OutlinedButton(
                    onClick = { controller.pushClipboardNow() },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("把当前剪贴板推给电脑") }
            }
        }
    }
}
