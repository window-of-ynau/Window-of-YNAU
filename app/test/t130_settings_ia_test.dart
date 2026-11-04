// t130：设置页 IA 防回归 —— 只保留 供应商 / 外观 / 隐私与记忆 / 关于。
//
// 谁把「对话设置」「模型与凭据」「套餐账号」加回设置页，这条会立刻红。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

Future<void> _pump(WidgetTester tester) async {
  setTestWindow(tester, 800, 900);
  await tester.pumpWidget(MaterialApp(
    home: SettingsPage(
      controller: ThemeController(store: MemorySettingsStore()),
      chatSettings: ChatSettingsController(),
    ),
  ));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('设置页分组顺序：供应商 → 外观 → 隐私与记忆 → 关于', (WidgetTester tester) async {
    await _pump(tester);
    final List<String> groups = <String>['供应商', '外观', '隐私与记忆', '关于'];
    for (final String group in groups) {
      expect(find.text(group), findsWidgets, reason: '缺少分组：$group');
    }
    // 顺序：y 坐标递增
    double lastY = -1;
    for (final String group in groups) {
      final double y = tester.getTopLeft(find.text(group).first).dy;
      expect(y, greaterThan(lastY), reason: '$group 的位置应在上一组之下');
      lastY = y;
    }
  });

  testWidgets('防回归：设置页不含旧 IA 文案', (WidgetTester tester) async {
    await _pump(tester);
    expect(find.text('对话设置'), findsNothing);
    expect(find.text('模型与凭据'), findsNothing);
    expect(find.text('套餐账号'), findsNothing);
    // 套餐登录状态也不应出现（只在对话页的模型选择器里）
    expect(find.textContaining('已登录'), findsNothing);
    expect(find.textContaining('未登录'), findsNothing);
  });

  testWidgets('设置页仍有唯一填 Key 的地方（供应商卡）', (WidgetTester tester) async {
    await _pump(tester);
    expect(find.text('供应商'), findsWidgets);
    expect(find.text('添加供应商'), findsWidgets);
  });
}
