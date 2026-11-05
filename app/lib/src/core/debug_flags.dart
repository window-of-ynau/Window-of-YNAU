// t153：调试/诊断入口的可见性开关（v0.9 alpha 前收口）。
//
// 为什么需要单独一个开关：
//   · 设置页的「[临时] 会话校验探针」会**真的从 WebView 请求校方页面**，
//     alpha 一旦被校方看到代价很大（issue #55）；
//   · 但排障时又必须有这些入口 —— 所以**只控制可见性、不删代码**；
//   · `kDebugMode` 不能在 widget 测试里改，所以加一个 `@visibleForTesting` 的覆盖位，
//     测试用 `DebugFlags.override = false` 模拟 release。
//
// **不在这里控制的东西**（它们是功能必需，不是调试入口）：
//   · `isCampusHost` 硬守卫（禁止非浏览器环境直连校方域名）；
//   · `window.__yunongGrabHtml()` / `window.__yunongCourseRowCount()`（抓取与就绪判定要用）。

import 'package:flutter/foundation.dart';

class DebugFlags {
  const DebugFlags._();

  /// 测试用覆盖：null = 跟随构建模式（`kDebugMode`）。
  @visibleForTesting
  static bool? override;

  /// 是否显示调试/诊断入口：debug 构建显示，release 隐藏。
  static bool get showDebugEntries => override ?? kDebugMode;

  /// 测试收尾用：恢复正常判定。
  @visibleForTesting
  static void reset() {
    override = null;
  }
}
