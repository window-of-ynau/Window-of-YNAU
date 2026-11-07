// t125：DeepSeek 模型名按官方文档修正（deepseek-flash / deepseek-v4-pro）+ 旧名静默迁移。
//
// 文档来源见 `lib/src/core/provider_store.dart` 顶部注释（api-docs.deepseek.com，2026-11 版）。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/provider_store.dart';
import 'package:yunong/src/theme/theme_controller.dart';

void main() {
  group('一、内置预设 = 文档值', () {
    test('名称 / base_url / 模型列表 / 默认模型', () {
      expect(DeepSeekPreset.name, 'DeepSeek');
      expect(DeepSeekPreset.baseUrl, 'https://api.deepseek.com');
      expect(DeepSeekPreset.models, <String>['deepseek-flash', 'deepseek-v4-pro']);
      expect(DeepSeekPreset.defaultModel, 'deepseek-flash');
      // Anthropic 兼容端点（协议由 Rust 自动解析，客户端只如实传 base_url）
      expect(DeepSeekPreset.anthropicBaseUrl, 'https://api.deepseek.com/anthropic');
      expect(DeepSeekPreset.isAnthropicEndpoint('https://api.deepseek.com/anthropic'), isTrue);
      expect(DeepSeekPreset.isAnthropicEndpoint('https://api.deepseek.com'), isFalse);
    });

    test('ProviderStore 默认**不预置** Provider；选中预设后才生效（t168）', () async {
      final ProviderStore store = ProviderStore(store: MemorySettingsStore());
      expect(store.hasConfigured, isFalse);
      expect(store.activeModels, isEmpty);
      await store.selectPreset(ProviderPreset.deepseek);
      expect(store.active.name, ProviderPreset.deepseek.displayName);
      expect(store.active.baseUrl, ProviderPreset.deepseek.baseUrl);
      expect(store.activeModels, <String>[ProviderPreset.deepseek.modelId]);
    });

    test('模型列表只有文档里的两个（不臆造）', () {
      expect(DeepSeekPreset.models.length, 2);
      expect(DeepSeekPreset.models, isNot(contains('deepseek-chat')));
      expect(DeepSeekPreset.models, isNot(contains('deepseek-reasoner')));
    });
  });

  group('二/三、旧名静默迁移到 deepseek-flash', () {
    test('migrateModelName：本项目旧名 → deepseek-flash', () {
      expect(migrateModelName('deepseek-chat').model, 'deepseek-flash');
      expect(migrateModelName('deepseek-chat').migratedFrom, 'deepseek-chat');
      expect(migrateModelName('deepseek-reasoner').model, 'deepseek-flash');
      expect(migrateModelName('deepseek-reasoner').migratedFrom, 'deepseek-reasoner');
    });

    test('文档旧名（v4-flash / vision-exp）→ deepseek-flash', () {
      expect(migrateModelName('deepseek-v4-flash').model, 'deepseek-flash');
      expect(migrateModelName('deepseek-v4-flash-vision-exp').model, 'deepseek-flash');
    });

    test('新名不动；空值回落默认', () {
      expect(migrateModelName('deepseek-v4-pro').migratedFrom, isNull);
      expect(migrateModelName('').model, DeepSeekPreset.defaultModel);
    });

    test('已存配置里的旧模型列表被迁移并记录一行诊断', () async {
      final MemorySettingsStore settings = MemorySettingsStore();
      await settings.set(
        ProviderStore.storageKey,
        '{"active":"default","list":[{"id":"default","name":"默认供应商",'
        '"base_url":"https://api.deepseek.com","models":["deepseek-chat","deepseek-reasoner"]}]}',
      );
      final ProviderStore store = ProviderStore(store: settings);
      await store.load();

      expect(store.activeModels, <String>['deepseek-flash'], reason: '两个旧名合并成一个新名且去重');
      expect(store.modelMigrationNote, contains('deepseek-chat'));
      expect(store.modelMigrationNote, contains('deepseek-flash'));
      // 迁移结果已回写，重启不会再迁
      final String? raw = await settings.get(ProviderStore.storageKey);
      expect(raw, contains('deepseek-flash'));
      expect(raw, isNot(contains('deepseek-chat')));
    });
  });
}
