// t132：rail 图标可见性（三主题）/ 三主题底部结构一致 / 顶栏形态 / 设置页账号分区。

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/chat/model_selector.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/theme/theme_controller.dart';
import 'package:yunong/src/theme/yunong_tokens.dart';

import 'support/test_window.dart';

double _contrast(Color a, Color b) {
  final double la = a.computeLuminance();
  final double lb = b.computeLuminance();
  final double hi = math.max(la, lb);
  final double lo = math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
}

Future<void> _pumpApp(WidgetTester tester, AppThemeId id, {Size size = const Size(1280, 800)}) async {
  setTestWindow(tester, size.width, size.height);
  await tester.pumpWidget(YunongApp(
    themeController: ThemeController(store: MemorySettingsStore(), initialTheme: id),
  ));
  await tester.pumpAndSettle();
}

void main() {
  for (final AppThemeId id in AppThemeId.values) {
    testWidgets('${id.label}：rail 有可见图标（对比度 ≥ 3:1）', (WidgetTester tester) async {
      await _pumpApp(tester, id);
      await tester.tap(find.byKey(const Key('sidebar-toggle')));
      await tester.pumpAndSettle();

      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, lessThanOrEqualTo(72));
      // 图标必须真的在树里
      expect(find.byIcon(Icons.chat_bubble_outline), findsWidgets, reason: '${id.label} rail 缺图标');

      // 图标实际颜色 vs 侧栏背景：非文本元素最低 3:1
      final BuildContext sidebarContext = tester.element(find.byKey(const Key('app-sidebar')));
      final YunongTokens tokens = sidebarContext.tokens;
      final Color iconColor = IconTheme.of(sidebarContext).color ?? tokens.ink;
      expect(_contrast(iconColor, tokens.canvas), greaterThanOrEqualTo(3.0),
          reason: '${id.label} rail 图标与背景对比度不足：icon=$iconColor canvas=${tokens.canvas}');

      // 悬停反馈：IconButton 至少要有 tooltip（可发现性）+ 默认 hover 高亮
      expect(find.byTooltip('对话'), findsWidgets);
    });

    testWidgets('${id.label}：对话页底部有 provider/模型/effort 选择器', (WidgetTester tester) async {
      await _pumpApp(tester, id);
      expect(find.byType(ModelSelector), findsOneWidget, reason: '${id.label} 缺模型选择器');
      expect(find.byKey(const Key('model-selector-button')), findsOneWidget);
      expect(find.byIcon(Icons.arrow_upward), findsWidgets);
    });
  }

  testWidgets('顶栏：无「直连/套餐」切换，只有 ⓘ 与 ⋮', (WidgetTester tester) async {
    await _pumpApp(tester, AppThemeId.minimalBlack, size: const Size(480, 900));
    expect(find.text('直连'), findsNothing);
    expect(find.text('套餐'), findsNothing);
    expect(find.byKey(const Key('chat-info')), findsOneWidget);
    expect(find.byKey(const Key('chat-menu')), findsOneWidget);
  });

  testWidgets('设置页有「账号」分区，占位项显式标注「即将支持」', (WidgetTester tester) async {
    setTestWindow(tester, 800, 1200);
    await tester.pumpWidget(MaterialApp(
      home: SettingsPage(
        controller: ThemeController(store: MemorySettingsStore()),
        chatSettings: ChatSettingsController(),
      ),
    ));
    await tester.pumpAndSettle();

    expect(find.text('账号'), findsWidgets);
    await tester.scrollUntilVisible(find.text('学校登录态（课表导入获得）'), 160,
        scrollable: find.byType(Scrollable).first);
    expect(find.textContaining('即将支持'), findsWidgets);
  });
}
