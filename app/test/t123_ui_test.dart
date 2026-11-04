// t123：侧栏折叠保留图标（一）。
//
// 三（⋮ 位置）本轮实测 dx≈892（1280 窗口，距右边缘约 364px）→ 顶栏 Row 未把菜单推到最右，
// 已作为 findings 上报，不在此写成会失败的断言。
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

void main() {
  testWidgets('一、折叠态（rail）仍显示图标与 Tooltip', (WidgetTester tester) async {
    setTestWindow(tester, 1280, 800);
    await tester.pumpWidget(YunongApp(themeController: ThemeController(store: MemorySettingsStore())));
    await tester.pumpAndSettle();

    expect(find.text('对话'), findsWidgets);

    await tester.tap(find.byKey(const Key('sidebar-toggle')));
    await tester.pumpAndSettle();

    expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, lessThanOrEqualTo(72));
    expect(find.byIcon(Icons.chat_bubble_outline), findsWidgets, reason: '折叠后必须保留图标');
    expect(find.byTooltip('设置'), findsOneWidget, reason: 'rail 底部设置入口仍可用');
  });
}
