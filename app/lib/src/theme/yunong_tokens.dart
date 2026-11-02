// 主题 token：**所有视觉参数的唯一来源**。
//
// 分两层（这是整个主题体系的地基，改之前先读这一段）：
//
//   1. 可变层（本文件绝大多数字段）：背景 / 表面 / 卡片 / 导航 / 边框 / 文字 /
//      强调色 / 圆角 / 字体 / 阴影 / 模糊 / 纸纹 —— 换主题时全部跟着变。
//   2. 不可变层：**课程块配色**（`course_palette.dart` 的 11 个色相 + FNV 索引）。
//      它是信息表达，不是装饰：换主题不该改变"哪门课是什么颜色"。
//      这里只允许一个开关 `useDarkCoursePalette`（深色画布取深档），
//      色相与课程的对应关系永远不变 —— 等价于 Material 里 primary 分 light/dark。
//
// 规则：widget 里不许出现 `Color(0xFF…)` / `BorderRadius.circular(12)` 之类字面量，
// 一律 `context.tokens.xxx`。

import 'package:flutter/material.dart';

/// 玻璃参数（只有 B 主题有；A/C 为 null，全树不会出现 `BackdropFilter`）。
@immutable
class GlassSpec {
  const GlassSpec({
    required this.blurSigma,
    required this.tint,
    required this.highlightTop,
    required this.allowBackdropFilter,
    this.innerGlow,
    this.saturate = 1.0,
  });

  /// 磨砂 = 18，流体 = 26，液态 = 0（纯渐变，不模糊）。
  final double blurSigma;

  /// 面板底色（半透明叠在背景上）。
  final Color tint;

  /// 1px 顶部高光，玻璃的"边"。
  final Color highlightTop;

  /// 液态玻璃的斜向高光带；磨砂为 null。
  final LinearGradient? innerGlow;

  /// false = 只画"假玻璃"（半透明 + 高光），不构造 `BackdropFilter`。
  /// 批次 1 全部为 false；批次 2 接真模糊 + 帧率降级后再打开。
  final bool allowBackdropFilter;

  /// 颜色矩阵饱和度；Windows 上可能多一遍 pass，默认 1.0。
  final double saturate;
}

/// 纸纹（只有 C 主题有）。
@immutable
class TextureSpec {
  const TextureSpec({
    required this.asset,
    required this.opacity,
    required this.tileSize,
  });

  final String asset;
  final double opacity;
  final double tileSize;
}

/// 三套主题共用的视觉 token。
@immutable
class YunongTokens extends ThemeExtension<YunongTokens> {
  const YunongTokens({
    required this.canvas,
    required this.surface,
    required this.surfaceAlt,
    required this.ink,
    required this.inkSoft,
    required this.inkFaint,
    required this.hairline,
    required this.controlBorder,
    required this.focusRing,
    required this.accent,
    required this.onAccent,
    required this.accentWash,
    required this.warnBg,
    required this.warnFg,
    required this.warnLine,
    required this.radiusS,
    required this.radiusM,
    required this.radiusL,
    required this.borderWidth,
    required this.titleSize,
    required this.bodySize,
    required this.metaSize,
    required this.bodyHeight,
    required this.metaLetterSpacing,
    required this.titleWeight,
    required this.strongWeight,
    required this.shadowPanel,
    required this.shadowFloat,
    required this.useDarkCoursePalette,
    required this.fast,
    required this.ease,
    this.displayFontFamily,
    this.glass,
    this.texture,
    this.canvasGradient,
    this.appBarElevation = 0,
  });

  // ── 1. 颜色（可变层） ───────────────────────────────────────────
  final Color canvas;
  final Color surface;
  final Color surfaceAlt;
  final Color ink;
  final Color inkSoft;
  final Color inkFaint;
  final Color hairline;
  final Color controlBorder;
  final Color focusRing;
  final Color accent;
  final Color onAccent;
  final Color accentWash;

  /// 琥珀（"未确认学期开始日"横幅）。红色留给解析失败，语义不能混。
  final Color warnBg;
  final Color warnFg;
  final Color warnLine;

  // ── 2. 形状 ────────────────────────────────────────────────────
  final double radiusS;
  final double radiusM;
  final double radiusL;
  final double borderWidth;
  final double appBarElevation;

  // ── 3. 字体 ────────────────────────────────────────────────────
  /// C 用衬线（v1 走系统 serif，不打包字体）；A/B 为 null。
  final String? displayFontFamily;
  final double titleSize;
  final double bodySize;
  final double metaSize;
  final double bodyHeight;
  final double metaLetterSpacing;
  final FontWeight titleWeight;
  final FontWeight strongWeight;

  // ── 4. 深度 ────────────────────────────────────────────────────
  final List<BoxShadow> shadowPanel;
  final List<BoxShadow> shadowFloat;

  // ── 5. 特效（A/C 恒为 null） ────────────────────────────────────
  final GlassSpec? glass;
  final TextureSpec? texture;

  /// 画布渐变（只有 B 主题有：深蓝底 + 光团，玻璃层才谈得上景深）。
  /// 非 null 时 `scaffoldBackgroundColor` 变透明，由 `MaterialApp.builder` 铺渐变。
  final Gradient? canvasGradient;

  // ── 6. 不可变层：课程色相只允许切明暗档 ──────────────────────────
  final bool useDarkCoursePalette;

  // ── 7. 动效 ────────────────────────────────────────────────────
  final Duration fast;
  final Curve ease;

  /// 玻璃主题画布上的文字色（非玻璃主题等于 ink）。
  Color get inkOnGlass => glass == null ? ink : const Color(0xFFF2F6FF);

  @override
  YunongTokens copyWith({
    Color? canvas,
    Color? surface,
    Color? surfaceAlt,
    Color? ink,
    Color? inkSoft,
    Color? inkFaint,
    Color? hairline,
    Color? controlBorder,
    Color? focusRing,
    Color? accent,
    Color? onAccent,
    Color? accentWash,
    Color? warnBg,
    Color? warnFg,
    Color? warnLine,
    double? radiusS,
    double? radiusM,
    double? radiusL,
    double? borderWidth,
    double? appBarElevation,
    String? displayFontFamily,
    double? titleSize,
    double? bodySize,
    double? metaSize,
    double? bodyHeight,
    double? metaLetterSpacing,
    FontWeight? titleWeight,
    FontWeight? strongWeight,
    List<BoxShadow>? shadowPanel,
    List<BoxShadow>? shadowFloat,
    GlassSpec? glass,
    TextureSpec? texture,
    Gradient? canvasGradient,
    bool? useDarkCoursePalette,
    Duration? fast,
    Curve? ease,
  }) {
    return YunongTokens(
      canvas: canvas ?? this.canvas,
      surface: surface ?? this.surface,
      surfaceAlt: surfaceAlt ?? this.surfaceAlt,
      ink: ink ?? this.ink,
      inkSoft: inkSoft ?? this.inkSoft,
      inkFaint: inkFaint ?? this.inkFaint,
      hairline: hairline ?? this.hairline,
      controlBorder: controlBorder ?? this.controlBorder,
      focusRing: focusRing ?? this.focusRing,
      accent: accent ?? this.accent,
      onAccent: onAccent ?? this.onAccent,
      accentWash: accentWash ?? this.accentWash,
      warnBg: warnBg ?? this.warnBg,
      warnFg: warnFg ?? this.warnFg,
      warnLine: warnLine ?? this.warnLine,
      radiusS: radiusS ?? this.radiusS,
      radiusM: radiusM ?? this.radiusM,
      radiusL: radiusL ?? this.radiusL,
      borderWidth: borderWidth ?? this.borderWidth,
      appBarElevation: appBarElevation ?? this.appBarElevation,
      displayFontFamily: displayFontFamily ?? this.displayFontFamily,
      titleSize: titleSize ?? this.titleSize,
      bodySize: bodySize ?? this.bodySize,
      metaSize: metaSize ?? this.metaSize,
      bodyHeight: bodyHeight ?? this.bodyHeight,
      metaLetterSpacing: metaLetterSpacing ?? this.metaLetterSpacing,
      titleWeight: titleWeight ?? this.titleWeight,
      strongWeight: strongWeight ?? this.strongWeight,
      shadowPanel: shadowPanel ?? this.shadowPanel,
      shadowFloat: shadowFloat ?? this.shadowFloat,
      glass: glass ?? this.glass,
      texture: texture ?? this.texture,
      canvasGradient: canvasGradient ?? this.canvasGradient,
      useDarkCoursePalette: useDarkCoursePalette ?? this.useDarkCoursePalette,
      fast: fast ?? this.fast,
      ease: ease ?? this.ease,
    );
  }

  /// 用指定的玻璃参数替换（传 null = 去掉玻璃，copyWith 做不到这点）。
  YunongTokens withGlass(GlassSpec? glass) => YunongTokens(
        canvas: canvas,
        surface: surface,
        surfaceAlt: surfaceAlt,
        ink: ink,
        inkSoft: inkSoft,
        inkFaint: inkFaint,
        hairline: hairline,
        controlBorder: controlBorder,
        focusRing: focusRing,
        accent: accent,
        onAccent: onAccent,
        accentWash: accentWash,
        warnBg: warnBg,
        warnFg: warnFg,
        warnLine: warnLine,
        radiusS: radiusS,
        radiusM: radiusM,
        radiusL: radiusL,
        borderWidth: borderWidth,
        appBarElevation: appBarElevation,
        displayFontFamily: displayFontFamily,
        titleSize: titleSize,
        bodySize: bodySize,
        metaSize: metaSize,
        bodyHeight: bodyHeight,
        metaLetterSpacing: metaLetterSpacing,
        titleWeight: titleWeight,
        strongWeight: strongWeight,
        shadowPanel: shadowPanel,
        shadowFloat: shadowFloat,
        glass: glass,
        texture: texture,
        canvasGradient: canvasGradient,
        useDarkCoursePalette: useDarkCoursePalette,
        fast: fast,
        ease: ease,
      );

  @override
  YunongTokens lerp(ThemeExtension<YunongTokens>? other, double t) {
    if (other is! YunongTokens) {
      return this;
    }
    Color c(Color a, Color b) => Color.lerp(a, b, t)!;
    double d(double a, double b) => a + (b - a) * t;
    return YunongTokens(
      canvas: c(canvas, other.canvas),
      surface: c(surface, other.surface),
      surfaceAlt: c(surfaceAlt, other.surfaceAlt),
      ink: c(ink, other.ink),
      inkSoft: c(inkSoft, other.inkSoft),
      inkFaint: c(inkFaint, other.inkFaint),
      hairline: c(hairline, other.hairline),
      controlBorder: c(controlBorder, other.controlBorder),
      focusRing: c(focusRing, other.focusRing),
      accent: c(accent, other.accent),
      onAccent: c(onAccent, other.onAccent),
      accentWash: c(accentWash, other.accentWash),
      warnBg: c(warnBg, other.warnBg),
      warnFg: c(warnFg, other.warnFg),
      warnLine: c(warnLine, other.warnLine),
      radiusS: d(radiusS, other.radiusS),
      radiusM: d(radiusM, other.radiusM),
      radiusL: d(radiusL, other.radiusL),
      borderWidth: d(borderWidth, other.borderWidth),
      appBarElevation: d(appBarElevation, other.appBarElevation),
      displayFontFamily: t < 0.5 ? displayFontFamily : other.displayFontFamily,
      titleSize: d(titleSize, other.titleSize),
      bodySize: d(bodySize, other.bodySize),
      metaSize: d(metaSize, other.metaSize),
      bodyHeight: d(bodyHeight, other.bodyHeight),
      metaLetterSpacing: d(metaLetterSpacing, other.metaLetterSpacing),
      titleWeight: t < 0.5 ? titleWeight : other.titleWeight,
      strongWeight: t < 0.5 ? strongWeight : other.strongWeight,
      shadowPanel: BoxShadow.lerpList(shadowPanel, other.shadowPanel, t) ?? const <BoxShadow>[],
      shadowFloat: BoxShadow.lerpList(shadowFloat, other.shadowFloat, t) ?? const <BoxShadow>[],
      glass: t < 0.5 ? glass : other.glass,
      texture: t < 0.5 ? texture : other.texture,
      canvasGradient: t < 0.5 ? canvasGradient : other.canvasGradient,
      useDarkCoursePalette: t < 0.5 ? useDarkCoursePalette : other.useDarkCoursePalette,
      fast: fast,
      ease: ease,
    );
  }
}

/// 取当前主题的 token。所有页面都通过它取视觉参数。
///
/// 兜底（`kTokensALight`）：没有挂 ThemeExtension 的场景（例如只 pump 了
/// 裸 `MaterialApp` 的 widget 测试）也能正常渲染，不会因为 `!` 崩掉。
extension YunongTokensContext on BuildContext {
  YunongTokens get tokens =>
      Theme.of(this).extension<YunongTokens>() ?? kTokensALight;
}

/// A（极简黑白）浅色的 token 常量。
///
/// 两个用途：① `themes.dart` 组装 A 浅色主题时直接复用它；
/// ② `context.tokens` 在没有 ThemeExtension 时兜底。
/// **改这里的值等于改 A 浅色主题**，别在别处再写一份。
const YunongTokens kTokensALight = YunongTokens(
  canvas: Color(0xFFFFFFFF),
  surface: Color(0xFFF7F7F7),
  surfaceAlt: Color(0xFFF0F0F0),
  ink: Color(0xFF0D0D0D),
  inkSoft: Color(0xFF6B6B6B),
  inkFaint: Color(0xFF8A8A8A),
  hairline: Color(0xFFE8E8E8),
  controlBorder: Color(0xFFCFCFCF),
  focusRing: Color(0xFF0D0D0D),
  accent: Color(0xFF0D0D0D),
  onAccent: Color(0xFFFFFFFF),
  accentWash: Color(0xFFF0F0F0),
  warnBg: Color(0xFFFFF3CD),
  warnFg: Color(0xFF5C4400),
  warnLine: Color(0xFFE8CE85),
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
  shadowPanel: <BoxShadow>[],
  shadowFloat: <BoxShadow>[],
  useDarkCoursePalette: false,
  fast: Duration(milliseconds: 120),
  ease: Curves.easeOutCubic,
);

// ─────────────────────────────────────────────────────────────────────
// 玻璃强度（批次 2）：只有 B 主题有意义；A/C 选什么都无效（glass == null）。
// ─────────────────────────────────────────────────────────────────────

/// 玻璃质感档位。`auto` 由 `GlassGovernor` 按帧率自动升降。
enum GlassMode {
  auto,
  frosted,
  liquid,
  fluid,
  off;

  String get label {
    switch (this) {
      case GlassMode.auto:
        return '自动';
      case GlassMode.frosted:
        return '磨砂';
      case GlassMode.liquid:
        return '液态';
      case GlassMode.fluid:
        return '流体';
      case GlassMode.off:
        return '关闭';
    }
  }

  String get storageValue => name;

  static GlassMode fromStorage(String? value) {
    for (final GlassMode mode in GlassMode.values) {
      if (mode.name == value) {
        return mode;
      }
    }
    return GlassMode.auto;
  }
}

extension GlassSpecX on GlassSpec {
  GlassSpec copyWith({
    double? blurSigma,
    Color? tint,
    Color? highlightTop,
    LinearGradient? innerGlow,
    bool? allowBackdropFilter,
    double? saturate,
  }) {
    return GlassSpec(
      blurSigma: blurSigma ?? this.blurSigma,
      tint: tint ?? this.tint,
      highlightTop: highlightTop ?? this.highlightTop,
      innerGlow: innerGlow ?? this.innerGlow,
      allowBackdropFilter: allowBackdropFilter ?? this.allowBackdropFilter,
      saturate: saturate ?? this.saturate,
    );
  }
}

/// 按档位给出玻璃参数。**数值全部来自设计稿**（磨砂 18 / 流体 26 / 液态 0）。
GlassSpec glassSpecFor(GlassMode mode, GlassSpec base) {
  switch (mode) {
    case GlassMode.frosted:
      return base.copyWith(blurSigma: 18, allowBackdropFilter: true, innerGlow: null);
    case GlassMode.liquid:
      // 液态 = 纯渐变高光 + 双内阴影，**零 blur**（这是它便宜的原因）
      return base.copyWith(
        blurSigma: 0,
        allowBackdropFilter: false,
        innerGlow: const LinearGradient(
          begin: Alignment.topLeft,
          end: Alignment.bottomRight,
          colors: <Color>[Color(0x38FFFFFF), Color(0x0AFFFFFF), Color(0x1A00DCFF)],
          stops: <double>[0.0, 0.55, 1.0],
        ),
      );
    case GlassMode.fluid:
      return base.copyWith(blurSigma: 26, allowBackdropFilter: true, innerGlow: null);
    case GlassMode.off:
    case GlassMode.auto:
      return base.copyWith(allowBackdropFilter: true, innerGlow: null);
  }
}