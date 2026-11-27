// #77：设置→供应商→「添加供应商」弹窗接入内置预设（OpenAI / Anthropic / DeepSeek）。
//
// 覆盖：预设清单与 BaseUrl、点选即填 name/baseUrl/modelId、已添加标「已添加」、
// 保存后落成稳定 id `preset-<id>`、自定义流程不受影响、编辑态不显示预设。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/provider_store.dart';
import 'package:yunong/src/settings/provider_card.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

Future<ProviderStore> _pumpCard(WidgetTester tester, {ProviderStore? store}) async {
  setTestWindow(tester, 1000, 900);
  final ProviderStore s = store ?? ProviderStore(store: MemorySettingsStore());
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: SingleChildScrollView(child: ProviderCard(store: s, secrets: null)),
    ),
  ));
  await tester.pumpAndSettle();
  return s;
}

Future<void> _openAddDialog(WidgetTester tester) async {
  await tester.tap(find.text('添加供应商'));
  await tester.pumpAndSettle();
}

String _fieldText(WidgetTester tester, String key) =>
    tester.widget<TextField>(find.byKey(Key(key))).controller!.text;

void main() {
  testWidgets('弹窗顶部列出 3 个内置预设（显示名 + BaseUrl）', (WidgetTester tester) async {
    await _pumpCard(tester);
    await _openAddDialog(tester);

    expect(find.text('内置预设'), findsOneWidget);
    for (final String id in <String>['openai', 'anthropic', 'deepseek']) {
      expect(find.byKey(Key('preset-$id')), findsOneWidget, reason: '缺少预设 $id');
    }
    expect(find.text('GPT6.1Sol'), findsOneWidget);
    expect(find.text('Claude Sonnet 5.5'), findsOneWidget);
    expect(find.text('deepseek-flash'), findsOneWidget);
    expect(find.text('https://api.openai.com'), findsOneWidget);
    expect(find.text('https://api.anthropic.com'), findsOneWidget);
    expect(find.text('https://api.deepseek.com'), findsOneWidget);
  });

  testWidgets('点选预设即填 name/baseUrl/modelId（三种预设都验）', (WidgetTester tester) async {
    await _pumpCard(tester);
    await _openAddDialog(tester);

    const Map<String, ({String name, String base, String model})> expected =
        <String, ({String name, String base, String model})>{
      'openai': (name: 'GPT6.1Sol', base: 'https://api.openai.com', model: 'GPT6.1Sol'),
      'anthropic': (
        name: 'Claude Sonnet 5.5',
        base: 'https://api.anthropic.com',
        model: 'Claude Sonnet 5.5'
      ),
      'deepseek': (
        name: 'deepseek-flash',
        base: 'https://api.deepseek.com',
        model: 'deepseek-flash'
      ),
    };

    for (final MapEntry<String, ({String name, String base, String model})> e
        in expected.entries) {
      await tester.tap(find.byKey(Key('preset-${e.key}')));
      await tester.pumpAndSettle();
      expect(_fieldText(tester, 'provider-name'), e.value.name);
      expect(_fieldText(tester, 'provider-base-url'), e.value.base);
      expect(_fieldText(tester, 'provider-models'), e.value.model);
    }
  });

  testWidgets('保存预设 → 稳定 id / 名称 / BaseUrl / 模型；重开标「已添加」且不可再点',
      (WidgetTester tester) async {
    final ProviderStore store = await _pumpCard(tester);
    await _openAddDialog(tester);

    await tester.tap(find.byKey(const Key('preset-deepseek')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('provider-api-key')), 'sk-deepseek-test');
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();

    expect(store.providers.length, 1);
    final ProviderConfig saved = store.providers.single;
    expect(saved.id, 'preset-deepseek');
    expect(saved.name, 'deepseek-flash');
    expect(saved.baseUrl, 'https://api.deepseek.com');
    expect(saved.models, <String>['deepseek-flash']);
    expect(store.activeId, 'preset-deepseek');

    // 再次打开：DeepSeek 标「已添加」且置灰；其它两个仍可点。
    await _openAddDialog(tester);
    expect(find.byKey(const Key('preset-added-deepseek')), findsOneWidget);
    expect(find.text('已添加'), findsOneWidget);
    expect(tester.widget<ListTile>(find.byKey(const Key('preset-deepseek'))).enabled, isFalse);
    expect(tester.widget<ListTile>(find.byKey(const Key('preset-openai'))).enabled, isTrue);
    expect(tester.widget<ListTile>(find.byKey(const Key('preset-anthropic'))).enabled, isTrue);

    // 点已经添加的预设不生效：名称仍是空的。
    await tester.tap(find.byKey(const Key('preset-deepseek')));
    await tester.pumpAndSettle();
    expect(_fieldText(tester, 'provider-name'), isEmpty);
  });

  testWidgets('自定义流程保持：不选预设手填四项 → 随机 id、模型按逗号拆分',
      (WidgetTester tester) async {
    final ProviderStore store = await _pumpCard(tester);
    await _openAddDialog(tester);

    await tester.enterText(find.byKey(const Key('provider-name')), 'MyProvider');
    await tester.enterText(find.byKey(const Key('provider-base-url')), 'https://api.example.com');
    await tester.enterText(find.byKey(const Key('provider-models')), 'm1, m2');
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();

    final ProviderConfig saved = store.providers.single;
    expect(saved.name, 'MyProvider');
    expect(saved.baseUrl, 'https://api.example.com');
    expect(saved.models, <String>['m1', 'm2']);
    expect(saved.id.startsWith('preset-'), isFalse, reason: '自定义不应占用预设 id');
  });

  testWidgets('编辑已有配置时不显示内置预设（避免误认为改名）', (WidgetTester tester) async {
    final ProviderStore store = ProviderStore(store: MemorySettingsStore());
    await store.add(const ProviderConfig(
      id: 'custom-1',
      name: '已有供应商',
      baseUrl: 'https://api.example.com',
      models: <String>['m1'],
    ));
    await _pumpCard(tester, store: store);

    await tester.tap(find.byTooltip('编辑'));
    await tester.pumpAndSettle();

    expect(find.text('编辑供应商'), findsOneWidget);
    expect(find.text('内置预设'), findsNothing);
    expect(find.byKey(const Key('preset-openai')), findsNothing);
  });
}
