// t86（A 方案）：设置页「对话设置」入口 + 共享状态（摘要立即更新）。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

void main() {
  testWidgets('设置页不再有「对话」分组（t130：IA 已合并到供应商）', (WidgetTester tester) async {
    final theme = ThemeController(store: MemorySettingsStore());
    final chat = ChatSettingsController();
    chat.settings.model = 'deepseek-flash';

    await tester.pumpWidget(MaterialApp(
      home: SettingsPage(controller: theme, chatSettings: chat),
    ));
    await tester.pumpAndSettle();

    // t130：旧 IA 全部移除，且不会再被打回来
    expect(find.text('对话设置'), findsNothing);
    expect(find.text('模型与凭据'), findsNothing);
    expect(find.text('套餐账号'), findsNothing);
    expect(find.textContaining('通道：BYOK 直连'), findsNothing);
    // 供应商仍在（唯一能填 Key 的地方）
    expect(find.text('供应商'), findsWidgets);
    // 共享状态本身仍可用（控制器层，不依赖设置页摘要）
    expect(chat.summary, contains('deepseek-flash'));
  });

  testWidgets('共享状态：apply 后控制器摘要立即更新（设置页不再展示摘要）', (WidgetTester tester) async {
    final theme = ThemeController(store: MemorySettingsStore());
    final chat = ChatSettingsController();

    await tester.pumpWidget(MaterialApp(
      home: SettingsPage(controller: theme, chatSettings: chat),
    ));
    await tester.pumpAndSettle();
    expect(chat.summary, contains('deepseek-flash'));

    final next = ChatSettings()
      ..channel = 'cloud'
      ..model = 'qwen-max'
      ..protocol = 'openai_responses';
    chat.apply(next);
    await tester.pumpAndSettle();

    expect(chat.summary, contains('qwen-max'));
    expect(chat.summary, contains('套餐通道'));
    expect(chat.summary, contains('OpenAI Responses'));
    // 对话页读到的是同一个实例
    expect(chat.settings.model, 'qwen-max');
  });
}
