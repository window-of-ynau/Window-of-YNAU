// 搜索抽象的 Dart 侧测试（fake backend）：四通道 + 诚实提示。
//
// 验收核心：未配置时 UI 必须出现常驻提示，且**不能谎称已搜索**。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/search_service.dart';

/// 与 Rust `resolve_source` 同语义的假后端（不依赖 DLL）。
class FakeSearchBackend implements SearchBackend {
  FakeSearchBackend({this.byokKeySet = false});

  bool byokKeySet;

  @override
  Map<String, dynamic> status({
    required String channel,
    required String protocol,
    required String dbPath,
  }) {
    final SearchSource source;
    if (channel == 'cloud') {
      source = protocol == 'openai_responses' ? SearchSource.builtIn : SearchSource.proxy;
    } else if (channel == 'byok') {
      source = byokKeySet ? SearchSource.byok : SearchSource.none;
    } else {
      source = SearchSource.none;
    }
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{
        'source': source.wire,
        'label': source.label,
        'available': source.available,
        'hint': source.available ? '' : '未配置联网搜索，模型无法获取实时信息',
      },
    };
  }
}

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
}

void main() {
  group('SearchService：四通道', () {
    test('套餐 + Responses → 内置', () async {
      final service = SearchService(backend: FakeSearchBackend());
      final status = await service.status(channel: 'cloud', protocol: 'openai_responses');
      expect(status.source, SearchSource.builtIn);
      expect(status.label, '内置');
      expect(status.available, isTrue);
      expect(status.hint, isEmpty);
    });

    test('套餐 + 其它协议 → 我方代理', () async {
      final service = SearchService(backend: FakeSearchBackend());
      for (final protocol in <String>['openai_completions', 'anthropic_messages']) {
        final status = await service.status(channel: 'cloud', protocol: protocol);
        expect(status.source, SearchSource.proxy, reason: protocol);
        expect(status.label, '我方代理');
        expect(status.available, isTrue);
      }
    });

    test('BYOK 已填自带 Key → 自带 Key', () async {
      final service = SearchService(backend: FakeSearchBackend(byokKeySet: true));
      final status = await service.status(channel: 'byok', protocol: 'openai_completions');
      expect(status.source, SearchSource.byok);
      expect(status.label, '自带 Key');
      expect(status.available, isTrue);
    });

    test('都没有 → 未配置 + 诚实提示', () async {
      final service = SearchService(backend: FakeSearchBackend());
      final status = await service.status(channel: 'byok', protocol: 'openai_completions');
      expect(status.source, SearchSource.none);
      expect(status.label, '未配置');
      expect(status.available, isFalse);
      expect(status.hint, contains('未配置联网搜索'));
    });

    test('后端报错 / 核心库不可用 → 诚实兜底为未配置', () async {
      final service = SearchService(backend: _BrokenBackend());
      final status = await service.status(channel: 'cloud', protocol: 'openai_responses');
      expect(status.available, isFalse);
      expect(status.label, '未配置');
    });
  });

  group('UI：未配置时的提示收进 ⓘ（t132）', () {
    testWidgets('BYOK 无 Key → ⓘ 弹窗里显示“未配置联网搜索”', (WidgetTester tester) async {
      final search = SearchService(backend: FakeSearchBackend());
      await tester.pumpWidget(MaterialApp(
        home: Scaffold(
          body: ChatPage(
            service: ChatService(backend: _NoopChatBackend()),
            searchService: search,
          ),
        ),
      ));
      await tester.pumpAndSettle();

      // t132：横幅已移除，信息在 ⓘ 里
      expect(find.textContaining('未配置联网搜索'), findsNothing);
      await tester.tap(find.byKey(const Key('chat-info')));
      await tester.pumpAndSettle();
      expect(find.textContaining('未配置联网搜索'), findsOneWidget);
      // 不谎称已搜索
      expect(find.textContaining('已搜索'), findsNothing);
    });

    testWidgets('套餐 + Responses → 显示“内置”且没有未配置提示', (WidgetTester tester) async {
      final search = SearchService(backend: FakeSearchBackend());
      await tester.pumpWidget(MaterialApp(
        home: Scaffold(
          body: ChatPage(
            service: ChatService(backend: _NoopChatBackend()),
            searchService: search,
            settings: ChatSettings()..channel = 'cloud',
          ),
        ),
      ));
      await tester.pumpAndSettle();

      // 默认协议是 openai_completions → 我方代理；切到 Responses 才是内置
      // t132：提示收进 ⓘ，弹窗里能看到搜索来源
      await tester.tap(find.byKey(const Key('chat-info')));
      await tester.pumpAndSettle();
      expect(find.textContaining('联网搜索：已配置'), findsOneWidget);
      expect(find.textContaining('未配置联网搜索'), findsNothing);
    });
  });
}

/// 永远报错的后端（模拟核心库不可用）。
class _BrokenBackend implements SearchBackend {
  @override
  Map<String, dynamic> status({
    required String channel,
    required String protocol,
    required String dbPath,
  }) =>
      <String, dynamic>{'ok': false, 'code': 'INTERNAL', 'message': 'no core'};
}
