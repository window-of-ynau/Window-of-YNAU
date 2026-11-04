// t115：设置页 IA = provider 维度；旧 Key 自动迁移到默认 provider；模型/effort 在对话页。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/model_selector.dart';
import 'package:yunong/src/core/provider_store.dart';
import 'package:yunong/src/core/secret_service.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

class _SeededSecrets implements SecretBackend {
  _SeededSecrets({this.legacy = true});

  final bool legacy;
  int saves = 0;

  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) {
    saves += 1;
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef'},
    };
  }

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) => <String, dynamic>{
        'ok': true,
        'data': <String, dynamic>{
          'set': key == SecretService.byokKeyName && legacy,
          'masked': key == SecretService.byokKeyName && legacy ? 'sk-****cdef' : '',
        },
      };

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
}

void main() {
  group('旧 Key 迁移（用户不需要重填）', () {
    test('默认 provider 沿用老的 byok_api_key', () {
      const ProviderConfig defaultProvider =
          ProviderConfig(id: ProviderStore.defaultProviderId, name: '默认', baseUrl: 'https://x');
      expect(defaultProvider.secretKeyName, SecretService.byokKeyName);
    });

    test('已存旧 Key → legacyKeyAvailable() 为 true，且不会触发任何保存', () async {
      final backend = _SeededSecrets();
      final store = ProviderStore(
        store: MemorySettingsStore(),
        secrets: SecretService(backend: backend, dbPath: '/tmp/t115'),
      );
      expect(await store.legacyKeyAvailable(), isTrue);
      expect(backend.saves, 0, reason: '迁移只读，不该重新写入 Key');
    });

    test('没有旧 Key → false（正常引导填写）', () async {
      final store = ProviderStore(
        store: MemorySettingsStore(),
        secrets: SecretService(backend: _SeededSecrets(legacy: false), dbPath: '/tmp/t115'),
      );
      expect(await store.legacyKeyAvailable(), isFalse);
    });

    test('provider 列表可增删并持久化到设置', () async {
      final SettingsStore settings = MemorySettingsStore();
      final store = ProviderStore(store: settings, secrets: SecretService(backend: _SeededSecrets(), dbPath: '/tmp/t115'));
      await store.add(const ProviderConfig(id: 'p2', name: '自建', baseUrl: 'https://self.example'));
      expect(store.providers.length, 2);
      expect(store.activeId, 'p2');
      final String? raw = await settings.get(ProviderStore.storageKey);
      expect(raw, contains('self.example'));
      await store.remove('p2');
      expect(store.providers.length, 1);
    });
  });

  group('设置页 IA（只有供应商能填 Key）', () {
    testWidgets('设置页有「供应商」；Key 输入不再出现在账号卡里', (WidgetTester tester) async {
      await tester.pumpWidget(MaterialApp(
        home: SettingsPage(
          controller: ThemeController(store: MemorySettingsStore()),
          providerStore: ProviderStore(store: MemorySettingsStore()),
          secretService: SecretService(backend: _SeededSecrets(), dbPath: '/tmp/t115'),
        ),
      ));
      await tester.pumpAndSettle();

      expect(find.text('供应商'), findsOneWidget);
      expect(find.text('套餐账号'), findsOneWidget);
      // Key 行已从「模型与凭据」里移除（Key 只在供应商卡里）
      expect(find.text('BYOK 密钥'), findsNothing);
      // 协议只读：不可选
      expect(find.textContaining('当前生效：自动'), findsOneWidget);
    });
  });

  group('对话页：模型选择器（t123 改为三段弹层）', () {
    testWidgets('按钮显示 provider · 模型 · effort，点击弹出三段菜单', (WidgetTester tester) async {
      await tester.pumpWidget(const MaterialApp(home: Scaffold(body: ModelSelector())));
      await tester.pumpAndSettle();

      expect(find.byKey(const Key('model-selector-button')), findsOneWidget);
      expect(find.textContaining('effort'), findsWidgets);

      await tester.tap(find.byKey(const Key('model-selector-button')));
      await tester.pumpAndSettle();
      expect(find.text('供应商'), findsWidgets);
      expect(find.text('模型'), findsWidgets);
      expect(find.text('推理等级'), findsWidgets);
      expect(find.text('套餐通道'), findsWidgets);
    });
  });}
