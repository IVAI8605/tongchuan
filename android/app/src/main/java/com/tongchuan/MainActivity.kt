package com.tongchuan

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import com.tongchuan.ui.MainScreen
import com.tongchuan.ui.theme.TongchuanTheme

/**
 * 2026-11-19 UI 重构：Compose 宿主。
 * 生命周期转发给 {@link TcController}，界面全部由 Compose 渲染。
 */
class MainActivity : ComponentActivity() {

    private val controller = TcController(this)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        controller.onCreate(intent)
        setContent {
            TongchuanTheme {
                MainScreen(controller)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        controller.onNewIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        controller.onResume()
    }

    override fun onPause() {
        super.onPause()
        controller.onPause()
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        controller.onActivityResult(requestCode, resultCode, data)
    }
}
