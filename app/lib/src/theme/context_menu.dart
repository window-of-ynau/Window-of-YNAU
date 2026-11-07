// 文本选择菜单（右键 / 长按）的**实心**保障（t96）。
//
// 为什么需要单独处理：`AdaptiveTextSelectionToolbar` 是 Material 的独立浮层，
// **不受 `dialogTheme` 控制**；玻璃主题下它会半透明，底下的字透上来，看不清菜单项。
//
// 方案：在 `MaterialApp.builder` 里把整棵子树（含 Overlay）包进一个「surface 全不透明」的 Theme。
// 选它而不是自定义 `contextMenuBuilder`：本仓库的 Flutter SDK 版本没有该参数（实测 analyze 报
// `contextMenuBuilder isn't defined`），而 builder 包 Theme 覆盖范围等价（工具栏就在 Navigator 的 Overlay 里），
// 且只改一处、对所有 TextField/SelectableText 同时生效。

import 'package:flutter/material.dart';

/// 【t149】给某个页面子树套一层「浮层不透明」的主题作用域。
///
/// 为什么需要页面级：全局 `MaterialApp.builder` 那层已经 solid 化，但**菜单是在页面子树里
/// 触发的**（如课表页「学期」输入框的文本选择工具栏），它读的是**该页面 context 的主题**。
/// 只要中间有任何一层把主题换成带 alpha 的版本（页面为配色自建 `Theme`、或父级用了
/// `Material(type: transparency)` 再叠渐变），菜单就会透底。用这个作用域兜住，页面怎么套都不会透。
class OpaqueMenuScope extends StatelessWidget {
  const OpaqueMenuScope({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) =>
      Theme(data: solidSurfaceTheme(Theme.of(context)), child: child);
}

///
/// t149：不只 `colorScheme.*` —— 文本选择工具栏 / `showMenu` / `PopupMenu` 在部分 Flutter 版本里
/// 取的是 `canvasColor` / `cardColor` / `menuTheme.background` / `popupMenuTheme.color`，
/// 这些字段若带 alpha（或为 `Colors.transparent`）就会**直接透出下层内容**。
/// 课表页长按「学期」输入框弹菜单正是走这条路（其它页面没触发到这些字段）。
ThemeData solidSurfaceTheme(ThemeData base) {
  final ColorScheme scheme = base.colorScheme;
  Color solid(Color color) => color.withValues(alpha: 1);
  final Color opaqueSurface = solid(scheme.surface);
  return base.copyWith(
    // 浮层默认取色（M2 路径）：透明会让菜单直接透底
    canvasColor: opaqueSurface,
    cardColor: opaqueSurface,
    dialogTheme: (base.dialogTheme).copyWith(backgroundColor: opaqueSurface),
    colorScheme: scheme.copyWith(
      surface: opaqueSurface,
      surfaceContainerHighest: solid(scheme.surfaceContainerHighest),
      surfaceContainerHigh: solid(scheme.surfaceContainerHigh),
      surfaceContainer: solid(scheme.surfaceContainer),
    ),
    // showMenu / PopupMenuButton 的默认背景
    popupMenuTheme: base.popupMenuTheme.copyWith(
      color: solid(scheme.surfaceContainerHigh),
    ),
    // 新 Menu 体系（MenuAnchor 等）的背景
    menuTheme: MenuThemeData(
      style: MenuStyle(
        backgroundColor: WidgetStatePropertyAll<Color>(solid(scheme.surfaceContainerHigh)),
      ),
    ),
  );
}
