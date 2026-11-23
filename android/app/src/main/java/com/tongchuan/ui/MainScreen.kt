package com.tongchuan.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.tongchuan.R
import com.tongchuan.TcController
import kotlinx.coroutines.delay

/**
 * 主界面（v1.2.0）：顶部标题 + 版本号 + 发送页右上角「扫码配对」图标；底部三页「发送 / 接收 / 设置」。
 * 「展示二维码」对话框在本层统一渲染，任意页触发都能弹出。
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MainScreen(controller: TcController) {
    var tab by remember { mutableStateOf(0) }
    var qrMenu by remember { mutableStateOf(false) }
    val ui = controller.getUi()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("局域网通传") },
                actions = {
                    // 发送页右上角：扫码配对 / 展示我的码
                    if (tab == 0) {
                        Box {
                            IconButton(onClick = { qrMenu = true }) {
                                Icon(
                                    painter = painterResource(R.drawable.ic_scan),
                                    contentDescription = "扫码配对",
                                )
                            }
                            DropdownMenu(expanded = qrMenu, onDismissRequest = { qrMenu = false }) {
                                DropdownMenuItem(
                                    text = { Text("扫对方的码") },
                                    onClick = {
                                        qrMenu = false
                                        controller.openScanner()
                                    },
                                )
                                DropdownMenuItem(
                                    text = { Text("展示我的码（让对方扫）") },
                                    onClick = {
                                        qrMenu = false
                                        controller.showPairDialog()
                                    },
                                )
                            }
                        }
                    }
                    Text(
                        text = if (ui.versionName.isEmpty()) "" else "v" + ui.versionName,
                        style = MaterialTheme.typography.labelMedium,
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
                    icon = { Text("收") }, label = { Text("接收") })
                NavigationBarItem(selected = tab == 2, onClick = { tab = 2 },
                    icon = { Text("设") }, label = { Text("设置") })
            }
        },
    ) { padding ->
        Box(modifier = Modifier.fillMaxSize().padding(padding)) {
            when (tab) {
                0 -> SendScreen(controller)
                1 -> ReceiveScreen(controller)
                else -> SettingsScreen(controller)
            }
        }
    }

    if (ui.pairDialogVisible) {
        PairQrDialog(controller)
    }
}

/**
 * 配对二维码对话框：状态来自控制器（二维码位图 + 过期时间戳），倒计时每秒读一次。
 * 发送页「展示我的码」与设置页「展示二维码」共用同一个对话框。
 */
@Composable
private fun PairQrDialog(controller: TcController) {
    val ui = controller.getUi()
    var now by remember { mutableLongStateOf(System.currentTimeMillis() / 1000) }
    LaunchedEffectOnce {
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
                    "让对方打开通传 → 右上角扫码图标 →「扫对方的码」扫这张码，扫到后直接发文件。"
                            + "有效期 10 分钟，只能配一台设备；关掉窗口不影响它继续可用。",
                    style = MaterialTheme.typography.bodySmall,
                )
                val qr = ui.pairQr
                if (qr != null) {
                    androidx.compose.foundation.Image(
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

/** LaunchedEffect(Unit) 的小封装，避免顶层多 import。 */
@Composable
private fun LaunchedEffectOnce(block: suspend () -> Unit) {
    androidx.compose.runtime.LaunchedEffect(Unit) { block() }
}
