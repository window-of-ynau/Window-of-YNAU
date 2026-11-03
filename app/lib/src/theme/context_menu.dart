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

/// 把主题里所有「浮层表面」颜色强制为 alpha=1（不透明）。
ThemeData solidSurfaceTheme(ThemeData base) {
  final ColorScheme scheme = base.colorScheme;
  Color solid(Color color) => color.withValues(alpha: 1);
  return base.copyWith(
    colorScheme: scheme.copyWith(
      surface: solid(scheme.surface),
      surfaceContainerHighest: solid(scheme.surfaceContainerHighest),
      surfaceContainerHigh: solid(scheme.surfaceContainerHigh),
      surfaceContainer: solid(scheme.surfaceContainer),
    ),
  );
}
