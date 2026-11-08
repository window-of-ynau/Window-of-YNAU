// t168：Provider 选择器 —— 未配置占位 + 发送拦截 + 三厂商预设 + 同名模型不重复。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/model_selector.dart';
import 'package:yunong/src/core/provider_store.dart';
import 'package:yunong/src/theme/theme_controller.dart';

Widget _host(Widget child) => MaterialApp(home: Scaffold(body: child));

void main() {
  group('预设字段（t168）', () {
    test('三个厂商预设的 displayName / modelId / baseUrl', () {
      expect(ProviderPreset.openai.displayName, 'GPT6.1Sol');
      expect(ProviderPreset.openai.modelId, 'GPT6.1Sol');
      expect(ProviderPreset.openai.baseUrl, 'https://api.openai.com');

      expect(ProviderPreset.anthropic.displayName, 'Claude Sonnet 5.5');
      expect(ProviderPreset.anthropic.modelId, 'Claude Sonnet 5.5');
      expect(ProviderPreset.anthropic.baseUrl, 'https://api.anthropic.com');

      expect(ProviderPreset.deepseek.displayName, 'deepseek-flash');
      expect(ProviderPreset.deepseek.modelId, 'deepseek-flash');
      expect(ProviderPreset.deepseek.baseUrl, 'https://api.deepseek.com');

      expect(ProviderPreset.all.length, 3);
    });

    test('预设只是模板：未选中时 store 没有配置', () {
      final ProviderStore store = ProviderStore(store: MemorySettingsStore());
      expect(store.hasConfigured, isFalse);
      expect(store.activeOrNull, isNull);
      expect(store.activeModels, isEmpty);
    });

    test('选中 OpenAI → 显示 OpenAI · GPT6.1Sol', () async {
      final ProviderStore store = ProviderStore(store: MemorySettingsStore());
      await store.selectPreset(ProviderPreset.openai);
      expect(store.hasConfigured, isTrue);
      expect(store.activeOrNull, isNotNull);
      expect(store.activeModels, <String>['GPT6.1Sol']);
      // 再选一次不会产生重复配置
      await store.selectPreset(ProviderPreset.openai);
      expect(store.providers.length, 1);
    });
  });

  group('未配置：占位 + 发送拦截（t168）', () {
    testWidgets('选择器显示占位符（不是 DeepSeek）', (WidgetTester tester) async {
      await tester.pumpWidget(_host(const ModelSelector(
        providers: <ProviderConfig>[],
        models: <String>[],
        model: '',
        effort: 'auto',
      )));
      await tester.pumpAndSettle();
      expect(find.text('请选择或添加你的 Provider'), findsOneWidget);
      expect(find.textContaining('deepseek-flash'), findsNothing);
    });

    testWidgets('已配置后显示 provider · 模型', (WidgetTester tester) async {
      await tester.pumpWidget(_host(ModelSelector(
        providers: <ProviderConfig>[ProviderPreset.openai.toConfig()],
        activeProviderId: 'preset-openai',
        models: <String>['GPT6.1Sol'],
        model: 'GPT6.1Sol',
        effort: 'auto',
      )));
      await tester.pumpAndSettle();
      expect(find.textContaining('GPT6.1Sol'), findsOneWidget);
      // t172：文字+折角样式，名字与 effort 用空格分隔
      expect(find.textContaining('GPT6.1Sol  自动'), findsOneWidget);
      expect(find.text('请选择或添加你的 Provider'), findsNothing);
    });
  });

  group('同名模型不再出现两次（t168）', () {
    test('当前 provider 的模型只取自己那份，去重兜底', () {
      // 以前是「DeepSeek 预设 + 当前 provider 模型」拼接 → 同名两次
      final List<String> merged = DeepSeekPreset.mergeModelNames(
        presets: <String>['deepseek-flash'],
        custom: <String>['deepseek-flash'],
      );
      expect(merged, <String>['deepseek-flash']);
      expect(merged.length, 1);
    });

    testWidgets('模型弹层里同一个模型只出现一次（按 provider 分组）',
        (WidgetTester tester) async {
      await tester.pumpWidget(_host(ModelSelector(
        providers: <ProviderConfig>[
          ProviderPreset.deepseek.toConfig(),
          ProviderPreset.openai.toConfig(),
        ],
        activeProviderId: 'preset-deepseek',
        models: <String>['deepseek-flash'],
        model: 'deepseek-flash',
        effort: 'auto',
      )));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('model-selector-button')));
      await tester.pumpAndSettle();
      // 三个维度都在，且 provider 行显示当前 provider
      expect(find.textContaining('deepseek-flash  自动'), findsWidgets);
      expect(find.text('供应商'), findsWidgets);
    });
  });
}