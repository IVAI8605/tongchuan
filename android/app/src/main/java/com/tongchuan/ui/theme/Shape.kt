package com.tongchuan.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Shapes

/**
 * M3 默认形状（extraSmall → extraLarge）。圆角只在卡片层出现，
 * 与重构前「圆角只在卡片这一层」的做法一致。
 */
val TongchuanShapes = Shapes(
    extraSmall = RoundedCornerShape(4),
    small = RoundedCornerShape(8),
    medium = RoundedCornerShape(12),
    large = RoundedCornerShape(16),
    extraLarge = RoundedCornerShape(28),
)
