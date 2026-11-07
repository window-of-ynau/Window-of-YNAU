// 测试窗口尺寸的统一入口（t120）。
//
// # 为什么不能用 `tester.binding.setSurfaceSize(...)`
//
// 实测（t119）：`setSurfaceSize(480, 900)` **不会改变 MediaQuery** ——
// 失败信息与 `MediaQuery.sizeOf(context)` 拿到的仍然是 `Size(800, 600)`。
// 后果是「窄窗/宽窗」断言其实一直在 800×600 下跑：布局即使真在 480 溢出也测不出来，
// 反过来也不会因为真实窄窗而失败 —— 即**假绿**。
//
// 正确写法是设置 view 的物理尺寸 + dpr，并在测试结束恢复：
//
// ```dart
// setTestWindow(tester, 480, 900);
// ```
//
// 该 helper 内部已 ddTearDown(tester.view.reset)，调用方不要再手动清理。
//
// 护栏：任何声明了窗口尺寸的用例，必须用 `setTestWindow`；用 `setSurfaceSize` 会让
// MediaQuery 不变（实测仍 800×600），断言假绿。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

/// 把测试窗口设成 [width] × [height]（逻辑像素，dpr 固定 1.0）。
///
/// 注意：必须在 `pumpWidget` 之前调用，且每个测试只调一次。
void setTestWindow(WidgetTester tester, double width, double height) {
  tester.view.physicalSize = Size(width, height);
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
}
