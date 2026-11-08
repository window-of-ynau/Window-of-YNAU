// t172：composer 重构验收 —— 一个框里三样、文字+折角、圆形发送键。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/provider_store.dart';
import 'package:yunong/src/core/search_service.dart';
import 'package:yunong/src/theme/theme_controller.dart';

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
}

class _NoopSearchBackend implements SearchBackend {
  @override
  Map<String, dynamic> status({
    required String channel,
    required String protocol,
    required String dbPath,
  }) =>
      <String, dynamic>{
        'source': 'none',
        'label': '未配置',
        'available': false,
        'hint': '',
      };
}

Future<void> _pumpChat(WidgetTester tester, {required ProviderStore store}) async {
  await tester.binding.setSurfaceSize(const Size(1200, 900));
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: _NoopChatBackend()),
        searchService: SearchService(backend: _NoopSearchBackend()),
        providerStore: store,
      ),
    ),
  ));
  await tester.pumpAndSettle();
}

void main() {
  group('composer（t172 #4）', () {
    testWidgets('未配置：显示占位文案，不显示任何默认厂商', (WidgetTester tester) async {
      final ProviderStore store = ProviderStore(store: MemorySettingsStore());
      await _pumpChat(tester, store: store);
      expect(find.textContaining('请选择或添加你的 Provider'), findsOneWidget);
      expect(find.textContaining('DeepSeek'), findsNothing);
      expect(find.textContaining('GPT6.1Sol'), findsNothing);
    });

    testWidgets('已配置：显示「名字  effort ⌄」且无边框填充按钮', (WidgetTester tester) async {
      final ProviderStore store = ProviderStore(store: MemorySettingsStore());
      await store.selectPreset(ProviderPreset.openai);
      await _pumpChat(tester, store: store);
      expect(find.textContaining('GPT6.1Sol'), findsOneWidget);
      expect(find.textContaining('自动'), findsOneWidget);
      expect(find.byIcon(Icons.expand_more), findsOneWidget);
    });

    testWidgets('发送键是圆形图标按钮，无「发送」文字', (WidgetTester tester) async {
      final ProviderStore store = ProviderStore(store: MemorySettingsStore());
      await store.selectPreset(ProviderPreset.openai);
      await _pumpChat(tester, store: store);
      expect(find.byKey(const Key('send-button')), findsOneWidget);
      expect(find.text('发送'), findsNothing);
      final Size sendSize = tester.getSize(find.byKey(const Key('send-button')));
      expect(sendSize.width, closeTo(36, 1));
      expect(sendSize.height, closeTo(36, 1));
      expect(find.byIcon(Icons.arrow_upward), findsOneWidget);
    });

    testWidgets('输入框 / provider / 发送键在同一个 composer 容器内', (WidgetTester tester) async {
      final ProviderStore store = ProviderStore(store: MemorySettingsStore());
      await store.selectPreset(ProviderPreset.openai);
      await _pumpChat(tester, store: store);
      final Finder composer = find.byKey(const Key('composer'));
      expect(composer, findsOneWidget);
      for (final Finder child in <Finder>[
        find.byKey(const Key('composer-input')),
        find.byKey(const Key('model-selector-button')),
        find.byKey(const Key('send-button')),
      ]) {
        expect(find.ancestor(of: child, matching: composer), findsOneWidget,
            reason: '应父子关系在 composer 内');
      }
    });
  });
}