// 三套主题的 token 值 + ThemeData 组装。
//
// 数值来源：design/theme-proposals/参数表.md（含实测对比度）与
// design/theme-proposals/主题切换技术方案.md（§3 三套 token 表）。
// 改任何一个颜色前，先跑 `test/theme_tokens_test.dart` 的对比度守卫。

import 'package:flutter/material.dart';

import 'theme_controller.dart';
import 'yunong_tokens.dart';

/// 琥珀（语义色，三套主题共用；红色留给"解析失败"，两者不能混）。
const Color _warnBgLight = Color(0xFFFFF3CD);
const Color _warnFgLight = Color(0xFF5C4400);
const Color _warnLineLight = Color(0xFFE8CE85);
const Color _warnBgDark = Color(0xFF3A2F12);
const Color _warnFgDark = Color(0xFFF0DFA8);
const Color _warnLineDark = Color(0xFF6B5623);

// ─────────────────────────────────────────────────────────────────────
// A · 极简黑白（默认）：1px 线 + 灰度层级 + 字重承担层级，无阴影无彩色
// ─────────────────────────────────────────────────────────────────────
YunongTokens _tokensA(Brightness brightness) {
  final bool dark = brightness == Brightness.dark;
  if (!dark) {
    // 浅色是默认主题，直接复用常量（`context.tokens` 的兜底也是它，只有一份）
    return kTokensALight;
  }
  return const YunongTokens(
    canvas: const Color(0xFF0B0B0B),
    surface: const Color(0xFF131313),
    surfaceAlt: const Color(0xFF1C1C1C),
    ink: const Color(0xFFF5F5F5),
    inkSoft: const Color(0xFFA8A8A8),
    inkFaint: const Color(0xFF6E6E6E),
    hairline: const Color(0xFF262626),
    controlBorder: const Color(0xFF3A3A3A),
    focusRing: const Color(0xFFF5F5F5),
    accent: const Color(0xFFF5F5F5),
    onAccent: const Color(0xFF0B0B0B),
    accentWash: const Color(0xFF1C1C1C),
    warnBg: const Color(0xFF3A2F12),
    warnFg: const Color(0xFFF0DFA8),
    warnLine: const Color(0xFF6B5623),
    radiusS: 8,
    radiusM: 14,
    radiusL: 14,
    borderWidth: 1,
    titleSize: 18,
    bodySize: 14,
    metaSize: 11,
    bodyHeight: 1.72,
    metaLetterSpacing: 0.8,
    titleWeight: FontWeight.w600,
    strongWeight: FontWeight.w700,
    // A 的"层级"靠线与字重，不靠投影
    shadowPanel: const <BoxShadow>[],
    shadowFloat: const <BoxShadow>[],
    useDarkCoursePalette: true,
    fast: const Duration(milliseconds: 120),
    ease: Curves.easeOutCubic,
  );
}

// ─────────────────────────────────────────────────────────────────────
// B · 深蓝玻璃：深蓝画布 + 玻璃面板（批次 1 先用"假玻璃"，不构造 BackdropFilter）
// ─────────────────────────────────────────────────────────────────────
YunongTokens _tokensB(Brightness brightness) {
  final bool dark = brightness == Brightness.dark;
  if (!dark) {
    // 浅色变体（备选，未出图）：白玻璃 + 深蓝字
    return const YunongTokens(
      canvas: const Color(0xFFF2F6FF),
      surface: const Color(0xB8FFFFFF),
      surfaceAlt: const Color(0xFFE6EEFF),
      ink: const Color(0xFF0B1A33),
      inkSoft: const Color(0xFF3C5170),
      inkFaint: const Color(0xFF6B7F9E),
      hairline: const Color(0x1A0B1A33),
      controlBorder: const Color(0x240B1A33),
      focusRing: const Color(0xFF2E56CE),
      accent: const Color(0xFF2E56CE),
      onAccent: const Color(0xFFFFFFFF),
      accentWash: const Color(0x142E56CE),
      warnBg: _warnBgLight,
      warnFg: _warnFgLight,
      warnLine: _warnLineLight,
      radiusS: 10,
      radiusM: 16,
      radiusL: 18,
      borderWidth: 1,
      titleSize: 18,
      bodySize: 14,
      metaSize: 11,
      bodyHeight: 1.72,
      metaLetterSpacing: 0.6,
      titleWeight: FontWeight.w600,
      strongWeight: FontWeight.w700,
      shadowPanel: <BoxShadow>[
        BoxShadow(
          color: const Color(0x140B1A33),
          blurRadius: 24,
          offset: const Offset(0, 10),
        ),
      ],
      shadowFloat: <BoxShadow>[
        BoxShadow(
          color: const Color(0x1F0B1A33),
          blurRadius: 34,
          offset: const Offset(0, 16),
        ),
      ],
      glass: const GlassSpec(
        blurSigma: 18,
        tint: Color(0xB8FFFFFF),
        highlightTop: Color(0xCCFFFFFF),
        allowBackdropFilter: false,
      ),
      useDarkCoursePalette: false,
      fast: const Duration(milliseconds: 140),
      ease: Curves.easeOutCubic,
    );
  }
  return const YunongTokens(
    canvas: const Color(0xFF070D1C),
    surface: const Color(0x0DFFFFFF),
    surfaceAlt: const Color(0x1AFFFFFF),
    ink: const Color(0xFFE8EEFC),
    inkSoft: const Color(0xFFA9BFF0),
    inkFaint: const Color(0xFF7E92C4),
    hairline: const Color(0x1AFFFFFF),
    controlBorder: const Color(0x24FFFFFF),
    focusRing: const Color(0xFF3F68E4),
    accent: const Color(0xFF3F68E4),
    onAccent: const Color(0xFFFFFFFF),
    accentWash: const Color(0x2E3F68E4),
    warnBg: _warnBgDark,
    warnFg: _warnFgDark,
    warnLine: _warnLineDark,
    radiusS: 10,
    radiusM: 16,
    radiusL: 18,
    borderWidth: 1,
    titleSize: 18,
    bodySize: 14,
    metaSize: 11,
    bodyHeight: 1.72,
    metaLetterSpacing: 0.6,
    titleWeight: FontWeight.w600,
    strongWeight: FontWeight.w700,
    shadowPanel: <BoxShadow>[
      BoxShadow(
        color: const Color(0x4D000000),
        blurRadius: 34,
        offset: const Offset(0, 14),
      ),
    ],
    shadowFloat: <BoxShadow>[
      BoxShadow(
        color: const Color(0x6B000000),
        blurRadius: 40,
        offset: const Offset(0, 18),
      ),
    ],
    canvasGradient: const LinearGradient(
      begin: Alignment.topLeft,
      end: Alignment.bottomRight,
      colors: <Color>[Color(0xFF0E1C3A), Color(0xFF070D1C), Color(0xFF04070F)],
      stops: <double>[0.0, 0.55, 1.0],
    ),
    glass: const GlassSpec(
      blurSigma: 18,
      // rgba(12,22,44,.52)
      tint: Color(0x850C162C),
      // rgba(255,255,255,.18)
      highlightTop: Color(0x2EFFFFFF),
      allowBackdropFilter: false, // 批次 2 打开（含帧率降级）
    ),
    useDarkCoursePalette: true,
    fast: const Duration(milliseconds: 140),
    ease: Curves.easeOutCubic,
  );
}

// ─────────────────────────────────────────────────────────────────────
// C · 书本质感橙红：米白纸底 + 橘红强调 + 衬线标题 + 宽行距
// ─────────────────────────────────────────────────────────────────────
YunongTokens _tokensC(Brightness brightness) {
  final bool dark = brightness == Brightness.dark;
  return YunongTokens(
    // 纸色（Claude 公开参数 #faf9f5 / #f5f0e8 / #e8e0d2）
    canvas: dark ? const Color(0xFF1A1611) : const Color(0xFFFAF9F5),
    surface: dark ? const Color(0xFF241E17) : const Color(0xFFFFFEFC),
    surfaceAlt: dark ? const Color(0xFF2E271E) : const Color(0xFFF5F0E8),
    ink: dark ? const Color(0xFFF0E6D8) : const Color(0xFF141413),
    inkSoft: dark ? const Color(0xFFB9A88E) : const Color(0xFF6C6A64),
    inkFaint: dark ? const Color(0xFF8A7A64) : const Color(0xFF8E8B82),
    hairline: dark ? const Color(0xFF3A322A) : const Color(0xFFE6DFD8),
    controlBorder: dark ? const Color(0xFF4A3F33) : const Color(0xFFE6DFD8),
    focusRing: dark ? const Color(0xFFDA7756) : const Color(0xFFA9583E),
    // 强调色只做链接/选中/图标（白字 on #CC785C 仅 3.28:1，所以不做大面积填充）
    accent: dark ? const Color(0xFFDA7756) : const Color(0xFFA9583E),
    // 白字 on #DA7756 只有 3.11:1 → 深色下橙底用墨字（5.63:1）
    onAccent: dark ? const Color(0xFF1A1611) : const Color(0xFFFFFFFF),
    accentWash: dark ? const Color(0x33DA7756) : const Color(0xFFF1E7DE),
    // 主按钮：墨黑底 + 纸色字（18.43:1）
    ctaBg: dark ? const Color(0xFFF0E6D8) : const Color(0xFF141413),
    ctaFg: dark ? const Color(0xFF1A1611) : const Color(0xFFFAF9F5),
    // 正文比标题浅一档：长文阅读 10.34:1，不刺眼
    bodyText: dark ? const Color(0xFFD8CCBC) : const Color(0xFF3D3D3A),
    // 珊瑚：只用于 3px 竖条等装饰，不承载文字
    accentSoft: const Color(0xFFDA7756),
    // 书页排版：助手消息无气泡、用户消息为页边批注
    bookLayout: true,
    warnBg: dark ? _warnBgDark : _warnBgLight,
    warnFg: dark ? _warnFgDark : _warnFgLight,
    warnLine: dark ? _warnLineDark : _warnLineLight,
    // 8 控件 / 12 卡片 / 16 大卡
    radiusS: 8,
    radiusM: 12,
    radiusL: 16,
    borderWidth: 1,
    displayFontFamily: 'Georgia',
    titleSize: 19,
    bodySize: 15,
    metaSize: 11.5,
    // 中文需要比英文（1.55）更大的行距
    bodyHeight: 1.85,
    metaLetterSpacing: 0.8,
    // 衬线只用 400 字重（加粗即失真）
    titleWeight: FontWeight.w400,
    strongWeight: FontWeight.w600,
    // 零阴影：层次靠色块与留白
    shadowPanel: const <BoxShadow>[],
    shadowFloat: const <BoxShadow>[],
    texture: const TextureSpec(
      asset: 'assets/textures/paper_2px.png',
      opacity: 0.32,
      tileSize: 2,
    ),
    useDarkCoursePalette: dark,
    fast: const Duration(milliseconds: 120),
    ease: Curves.easeOutCubic,
  );
}
/// 取某套主题在某亮度下的 token。
YunongTokens tokensFor(AppThemeId id, Brightness brightness) {
  switch (id) {
    case AppThemeId.deepGlass:
      return _tokensB(brightness);
    case AppThemeId.paperCoral:
      return _tokensC(brightness);
    case AppThemeId.minimalBlack:
      return _tokensA(brightness);
  }
}

/// 组装 ThemeData：Material 只做"骨架"（涟漪、语义色、焦点），
/// 真正的视觉参数全部来自 [YunongTokens]。
ThemeData buildTheme(
  AppThemeId id,
  Brightness brightness, {
  GlassMode glassMode = GlassMode.auto,
}) {
  YunongTokens t = tokensFor(id, brightness);
  // 玻璃档位只影响 B（其它主题 glass == null，这里不动）
  if (t.glass != null) {
    t = t.withGlass(glassMode == GlassMode.off ? null : glassSpecFor(glassMode, t.glass!));
  }
  final ThemeData base = ThemeData(brightness: brightness, useMaterial3: true);
  // P1-4：**所有** 对话框统一实心背景（玻璃只用于外层装饰；表单/正文必须可读）
  final DialogThemeData solidDialog = DialogThemeData(
    backgroundColor: t.canvas,
    surfaceTintColor: Colors.transparent,
    barrierColor: Colors.black54,
  );
  final ColorScheme scheme = ColorScheme.fromSeed(
    seedColor: t.accent,
    brightness: brightness,
  ).copyWith(
    primary: t.accent,
    onPrimary: t.onAccent,
    primaryContainer: t.accentWash,
    onPrimaryContainer: t.ink,
    surface: t.canvas,
    onSurface: t.ink,
    onSurfaceVariant: t.inkSoft,
    surfaceContainerHighest: t.surfaceAlt,
    surfaceContainerHigh: t.surfaceAlt,
    outline: t.controlBorder,
    outlineVariant: t.hairline,
    error: brightness == Brightness.dark ? const Color(0xFFFFB4AB) : const Color(0xFFBA1A1A),
  );

  final List<String>? serifFallback = t.displayFontFamily == null
      ? null
      : const <String>['Source Han Serif SC', 'Songti SC', 'SimSun', 'serif'];

  TextStyle scaled(TextStyle? base, double size, FontWeight weight, double height) {
    return (base ?? const TextStyle()).copyWith(
      fontSize: size,
      fontWeight: weight,
      height: height,
      color: t.ink,
      letterSpacing: null,
    );
  }

  final TextTheme text = base.textTheme.copyWith(
    displaySmall: scaled(base.textTheme.displaySmall, t.titleSize + 8, t.titleWeight, 1.25),
    headlineSmall: scaled(base.textTheme.headlineSmall, t.titleSize + 2, t.titleWeight, 1.3),
    titleLarge: scaled(base.textTheme.titleLarge, t.titleSize, t.titleWeight, 1.3),
    titleMedium: scaled(base.textTheme.titleMedium, t.bodySize + 1, t.titleWeight, 1.35),
    bodyMedium: scaled(base.textTheme.bodyMedium, t.bodySize, FontWeight.w400, t.bodyHeight).copyWith(color: t.bodyText),
    bodyLarge: scaled(base.textTheme.bodyLarge, t.bodySize + 1, FontWeight.w400, t.bodyHeight).copyWith(color: t.bodyText),
    bodySmall: scaled(base.textTheme.bodySmall, t.metaSize + 1, FontWeight.w400, 1.45),
    labelSmall: scaled(base.textTheme.labelSmall, t.metaSize, FontWeight.w700, 1.4)
        .copyWith(letterSpacing: t.metaLetterSpacing),
    labelLarge: scaled(base.textTheme.labelLarge, t.bodySize - 1, t.strongWeight, 1.2),
  ).apply(
    fontFamily: t.displayFontFamily,
    fontFamilyFallback: serifFallback,
    bodyColor: t.ink,
    displayColor: t.ink,
  );

  return base.copyWith(
    colorScheme: scheme,
    // 有画布渐变的主题（B）让 Scaffold 透明，由 MaterialApp.builder 铺渐变；否则用纯色
    scaffoldBackgroundColor: t.canvasGradient == null ? t.canvas : Colors.transparent,
    canvasColor: t.canvas,
    cardColor: t.surface,
    dividerColor: t.hairline,
    textTheme: text,
    primaryTextTheme: text,
    extensions: <ThemeExtension<dynamic>>[t],
    appBarTheme: AppBarTheme(
      backgroundColor: t.glass == null ? t.canvas : t.surface,
      foregroundColor: t.ink,
      elevation: t.appBarElevation,
      scrolledUnderElevation: 0,
      centerTitle: false,
      titleTextStyle: text.titleLarge,
      iconTheme: IconThemeData(color: t.ink),
    ),
    drawerTheme: DrawerThemeData(backgroundColor: t.canvas, surfaceTintColor: t.surface),
    iconTheme: IconThemeData(color: t.ink, size: 20),
    dividerTheme: DividerThemeData(color: t.hairline, thickness: t.borderWidth, space: t.borderWidth),
    listTileTheme: ListTileThemeData(
      textColor: t.ink,
      iconColor: t.inkSoft,
      selectedColor: t.ink,
      selectedTileColor: t.accentWash,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(t.radiusM)),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style: FilledButton.styleFrom(
        backgroundColor: t.ctaBg,
        foregroundColor: t.ctaFg,
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(t.radiusS)),
        minimumSize: const Size(0, 36),
        textStyle: text.labelLarge,
      ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        foregroundColor: t.ink,
        side: BorderSide(color: t.controlBorder, width: t.borderWidth),
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(t.radiusS)),
        minimumSize: const Size(0, 36),
        textStyle: text.labelLarge,
      ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(
        foregroundColor: t.ink,
        shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(t.radiusS)),
        textStyle: text.labelLarge,
      ),
    ),
    switchTheme: SwitchThemeData(
      thumbColor: WidgetStateProperty.resolveWith((Set<WidgetState> states) {
        return states.contains(WidgetState.selected) ? t.onAccent : t.inkSoft;
      }),
      trackColor: WidgetStateProperty.resolveWith((Set<WidgetState> states) {
        return states.contains(WidgetState.selected) ? t.accent : t.surfaceAlt;
      }),
      trackOutlineColor: WidgetStatePropertyAll<Color>(t.controlBorder),
    ),
    inputDecorationTheme: InputDecorationTheme(
      isDense: true,
      filled: true,
      fillColor: t.surface,
      hintStyle: TextStyle(color: t.inkFaint),
      labelStyle: TextStyle(color: t.inkSoft),
      contentPadding: const EdgeInsets.symmetric(horizontal: 12, vertical: 12),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(t.radiusS),
        borderSide: BorderSide(color: t.controlBorder, width: t.borderWidth),
      ),
      focusedBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(t.radiusS),
        borderSide: BorderSide(color: t.focusRing, width: t.borderWidth + 0.5),
      ),
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(t.radiusS),
        borderSide: BorderSide(color: t.controlBorder, width: t.borderWidth),
      ),
    ),
    snackBarTheme: SnackBarThemeData(
      backgroundColor: t.ink,
      contentTextStyle: TextStyle(color: t.canvas, fontSize: t.bodySize),
      behavior: SnackBarBehavior.floating,
    ),
    progressIndicatorTheme: ProgressIndicatorThemeData(color: t.accent),
    // P1-4：全局对话框实心背景（记忆页/设置页/登录框等所有 showDialog 自动生效）
    dialogTheme: solidDialog,
    bottomSheetTheme: BottomSheetThemeData(backgroundColor: t.canvas, surfaceTintColor: Colors.transparent),
  );
}