// t127：⋮ 必须贴顶栏右侧（480 与 1280 两档）。
//
// 定位证据（实测）：见 `expect(reason:)` 输出的 topBarRect / menuRect。
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/chat/chat_top_bar.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

Future<void> _pump(WidgetTester tester, double w, double h) async {
  setTestWindow(tester, w, h);
  await tester.pumpWidget(YunongApp(themeController: ThemeController(store: MemorySettingsStore())));
  await tester.pumpAndSettle();
}

Future<void> _expectMenuAtRight(WidgetTester tester, double width) async {
  final Rect topBar = tester.getRect(find.byType(ChatTopBar));
  final Rect menu = tester.getRect(find.byKey(const Key('chat-menu')));
  final double gap = topBar.right - menu.right;
  expect(gap, lessThanOrEqualTo(24),
      reason: '${width}px：topBar=$topBar menu=$menu gap=$gap');
}

void main() {
  testWidgets('1280：⋮ 贴顶栏右侧', (WidgetTester tester) async {
    await _pump(tester, 1280, 800);
    await _expectMenuAtRight(tester, 1280);
  });

  testWidgets('480：⋮ 贴顶栏右侧', (WidgetTester tester) async {
    await _pump(tester, 480, 900);
    await _expectMenuAtRight(tester, 480);
  });
}
