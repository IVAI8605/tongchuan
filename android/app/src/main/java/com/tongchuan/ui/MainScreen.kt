package com.tongchuan.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.tongchuan.TcController

/**
 * 主界面骨架：顶部标题 + 四页底部导航。
 * 各页内容在后续任务里逐页补齐，这里先保证「能编译、能切换、控制器通」。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MainScreen(controller: TcController) {
    var tab by remember { mutableStateOf(0) }
    val ui = controller.getUi()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("局域网通传") },
                actions = {
                    Text(
                        text = if (ui.versionName.isEmpty()) "" else "v" + ui.versionName,
                        style = androidx.compose.material3.MaterialTheme.typography.labelMedium,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.padding(end = 12.dp),
                    )
                },
            )
        },
        bottomBar = {
            NavigationBar {
                NavigationBarItem(selected = tab == 0, onClick = { tab = 0 },
                    icon = { Text("发") }, label = { Text("发送") })
                NavigationBarItem(selected = tab == 1, onClick = { tab = 1 },
                    icon = { Text("设") }, label = { Text("设备") })
                NavigationBarItem(selected = tab == 2, onClick = { tab = 2 },
                    icon = { Text("剪") }, label = { Text("剪贴板") })
                NavigationBarItem(selected = tab == 3, onClick = { tab = 3 },
                    icon = { Text("收") }, label = { Text("接收") })
            }
        },
    ) { padding ->
        Box(modifier = Modifier.fillMaxSize().padding(padding)) {
            when (tab) {
                0 -> EmptyHint("发送页（建设中）")
                1 -> DevicesPlaceholder(controller)
                2 -> EmptyHint("剪贴板页（建设中）")
                else -> EmptyHint("接收页（建设中）")
            }
        }
    }
}

/**
 * 临时占位：只放一个「启动常驻接收」，用来在设备页做完之前验证控制器还活着。
 * 设备页任务会整个替换掉这里。
 */
@Composable
private fun DevicesPlaceholder(controller: TcController) {
    Column(modifier = Modifier.fillMaxSize().padding(16.dp)) {
        StatusCard(text = controller.getUi().statusText, ok = controller.getUi().statusOk)
        Button(onClick = { controller.startRecv() }) {
            Text("启动常驻接收")
        }
    }
}
