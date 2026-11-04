// t86（A 方案）：设置页「对话设置」入口 + 共享状态（摘要立即更新）。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

void main() {
  testWidgets('设置页有「对话」分组：卡片 + 摘要，点击能打开设置弹窗', (WidgetTester tester) async {
    final theme = ThemeController(store: MemorySettingsStore());
    final chat = ChatSettingsController();
    chat.settings.model = 'deepseek-flash';

    await tester.pumpWidget(MaterialApp(
      home: SettingsPage(controller: theme, chatSettings: chat),
    ));
    await tester.pumpAndSettle();

    expect(find.text('对话'), findsWidgets);
    expect(find.text('对话设置'), findsOneWidget);
    expect(find.textContaining('通道：BYOK 直连'), findsOneWidget);
    expect(find.textContaining('deepseek-flash'), findsOneWidget);

    await tester.tap(find.text('对话设置'));
    await tester.pumpAndSettle();
    // 弹窗内容与对话页是同一个组件
    expect(find.byType(ChatSettingsSection), findsOneWidget);
  });

  testWidgets('共享状态：apply 后设置页摘要立即更新（无需重新打开）', (WidgetTester tester) async {
    final theme = ThemeController(store: MemorySettingsStore());
    final chat = ChatSettingsController();

    await tester.pumpWidget(MaterialApp(
      home: SettingsPage(controller: theme, chatSettings: chat),
    ));
    await tester.pumpAndSettle();
    expect(find.textContaining('deepseek-flash'), findsOneWidget);

    final next = ChatSettings()
      ..channel = 'cloud'
      ..model = 'qwen-max'
      ..protocol = 'openai_responses';
    chat.apply(next);
    await tester.pumpAndSettle();

    expect(find.textContaining('qwen-max'), findsOneWidget);
    expect(find.textContaining('套餐通道'), findsOneWidget);
    expect(find.textContaining('OpenAI Responses'), findsOneWidget);
    // 对话页读到的是同一个实例
    expect(chat.settings.model, 'qwen-max');
  });
}
