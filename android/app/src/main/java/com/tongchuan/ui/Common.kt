package com.tongchuan.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.tongchuan.ui.theme.LocalExtendedColors

/** 各页共用的卡片容器：标题 + 内容，圆角只在这一层出现。 */
@Composable
fun SectionCard(
    title: String? = null,
    content: @Composable () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (title != null) {
                Text(
                    text = title,
                    style = MaterialTheme.typography.titleMedium,
                    color = MaterialTheme.colorScheme.onSurface,
                )
            }
            content()
        }
    }
}

/**
 * 状态卡片：按 ok 选「成功 / 错误」语义色。
 * 颜色来自主题扩展槽，不在页面里写十六进制。
 */
@Composable
fun StatusCard(text: String, ok: Boolean) {
    val ext = LocalExtendedColors.current
    Card(modifier = Modifier.fillMaxWidth()) {
        Text(
            text = text,
            style = MaterialTheme.typography.bodySmall,
            color = if (ok) ext.success else MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(16.dp),
        )
    }
}

/** 空态提示。 */
@Composable
fun EmptyHint(text: String, padding: PaddingValues = PaddingValues(16.dp)) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
        modifier = Modifier.fillMaxSize().padding(padding),
    )
}
