package com.tongchuan.ui.theme

import androidx.compose.ui.graphics.Color

/**
 * 2026-11-19 UI 重构：全部颜色只在这里出现一次。
 *
 * seed 沿用重构前的品牌墨绿 #1F6F5C，按 Material 3 的角色拆分（primary / secondary /
 * tertiary / error / surface / outline），深浅两套。重构前散在 MainActivity 里的 7 个
 * 硬编码色（#F4F5F3 / #FFFFFF / #1B1B1B / #6B6B6B / #1F6F5C / #2E7D32 / #9A5B23 /
 * #B3261E）按语义归位，界面代码不再出现十六进制。
 *
 * 新增的 Success / Warning 是 M3 没有的语义槽：原项目的「成功 / 警告」色相保留，
 * 避免状态行在重构后分不清绿与橙。
 */

// ---- seed ----
private val SeedGreen = Color(0xFF1F6F5C)

// ---- 浅色 ----
val md_light_primary = Color(0xFF1F6F5C)
val md_light_onPrimary = Color(0xFFFFFFFF)
val md_light_primaryContainer = Color(0xFFA5F2DC)
val md_light_onPrimaryContainer = Color(0xFF00201A)
val md_light_secondary = Color(0xFF4A6357)
val md_light_onSecondary = Color(0xFFFFFFFF)
val md_light_secondaryContainer = Color(0xFFCCE8DA)
val md_light_onSecondaryContainer = Color(0xFF072019)
val md_light_tertiary = Color(0xFF7A5900)
val md_light_onTertiary = Color(0xFFFFFFFF)
val md_light_tertiaryContainer = Color(0xFFFFDF9E)
val md_light_onTertiaryContainer = Color(0xFF261A00)
val md_light_error = Color(0xFFB3261E)
val md_light_onError = Color(0xFFFFFFFF)
val md_light_errorContainer = Color(0xFFF9DEDC)
val md_light_onErrorContainer = Color(0xFF410E0B)
val md_light_background = Color(0xFFFBFDF9)
val md_light_onBackground = Color(0xFF191C1A)
val md_light_surface = Color(0xFFFBFDF9)
val md_light_onSurface = Color(0xFF191C1A)
val md_light_surfaceVariant = Color(0xFFDDE5DD)
val md_light_onSurfaceVariant = Color(0xFF414942)
val md_light_outline = Color(0xFF717971)
val md_light_inverseSurface = Color(0xFF2E312F)
val md_light_inverseOnSurface = Color(0xFFF0F1EC)
val md_light_inversePrimary = Color(0xFF85D6BE)

// ---- 深色 ----
val md_dark_primary = Color(0xFF85D6BE)
val md_dark_onPrimary = Color(0xFF00382C)
val md_dark_primaryContainer = Color(0xFF005141)
val md_dark_onPrimaryContainer = Color(0xFFA5F2DC)
val md_dark_secondary = Color(0xFFB3CCBE)
val md_dark_onSecondary = Color(0xFF1F352B)
val md_dark_secondaryContainer = Color(0xFF354B41)
val md_dark_onSecondaryContainer = Color(0xFFCCE8DA)
val md_dark_tertiary = Color(0xFFE4C275)
val md_dark_onTertiary = Color(0xFF402D00)
val md_dark_tertiaryContainer = Color(0xFF5C4200)
val md_dark_onTertiaryContainer = Color(0xFFFFDF9E)
val md_dark_error = Color(0xFFF2B8B5)
val md_dark_onError = Color(0xFF601410)
val md_dark_errorContainer = Color(0xFF8C1D18)
val md_dark_onErrorContainer = Color(0xFFF9DEDC)
val md_dark_background = Color(0xFF111412)
val md_dark_onBackground = Color(0xFFE1E3DF)
val md_dark_surface = Color(0xFF111412)
val md_dark_onSurface = Color(0xFFE1E3DF)
val md_dark_surfaceVariant = Color(0xFF414942)
val md_dark_onSurfaceVariant = Color(0xFFC1C9C0)
val md_dark_outline = Color(0xFF8B938B)
val md_dark_inverseSurface = Color(0xFFE1E3DF)
val md_dark_inverseOnSurface = Color(0xFF2E312F)
val md_dark_inversePrimary = Color(0xFF1F6F5C)

/** 成功 / 警告两个 M3 之外的语义槽，保留重构前的色相。 */
data class ExtendedColors(
    val success: Color,
    val onSuccess: Color,
    val successContainer: Color,
    val onSuccessContainer: Color,
    val warning: Color,
    val onWarning: Color,
    val warningContainer: Color,
    val onWarningContainer: Color,
)

val LightExtendedColors = ExtendedColors(
    success = Color(0xFF2E7D32),
    onSuccess = Color(0xFFFFFFFF),
    successContainer = Color(0xFFA8E6A8),
    onSuccessContainer = Color(0xFF00210A),
    warning = Color(0xFF9A5B23),
    onWarning = Color(0xFFFFFFFF),
    warningContainer = Color(0xFFFFD9B0),
    onWarningContainer = Color(0xFF331B00),
)

val DarkExtendedColors = ExtendedColors(
    success = Color(0xFF86C98A),
    onSuccess = Color(0xFF00390F),
    successContainer = Color(0xFF1E5B25),
    onSuccessContainer = Color(0xFFA8E6A8),
    warning = Color(0xFFF0B47A),
    onWarning = Color(0xFF4A2A00),
    warningContainer = Color(0xFF6B4200),
    onWarningContainer = Color(0xFFFFD9B0),
)
