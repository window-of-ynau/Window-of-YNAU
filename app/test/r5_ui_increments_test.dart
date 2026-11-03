// R5 端侧增量字段：get_secret.broken / 本地搜索 query 上限 512。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/local_search_service.dart';
import 'package:yunong/src/core/secret_service.dart';

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
}

/// `get_secret` 返回 `broken:true`（密文存在但解不开）。
class _BrokenSecrets implements SecretBackend {
  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) =>
      <String, dynamic>{
        'ok': true,
        'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef', 'broken': false},
      };

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) => <String, dynamic>{
        'ok': true,
        'data': <String, dynamic>{
          'set': true,
          'masked': 'sk-****cdef',
          'degraded': false,
          'broken': true,
        },
      };

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
}

class _FakeLocalSearch implements LocalSearchBackend {
  @override
  List<Map<String, dynamic>> search({
    required String query,
    required int limit,
    required String dbPath,
  }) =>
      const <Map<String, dynamic>>[];
}

Future<void> _pump(WidgetTester tester, {SecretBackend? secrets}) async {
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: _NoopChatBackend()),
        secretService: SecretService(backend: secrets ?? _BrokenSecrets(), dbPath: '/tmp/r5.db'),
        localSearchService: LocalSearchService(backend: _FakeLocalSearch(), dbPath: '/tmp/r5.db'),
        dbPath: '/tmp/r5.db',
      ),
    ),
  ));
  await tester.pumpAndSettle();
}

void main() {
  test('SecretStatus 解析 broken，usable 为 false', () {
    final status = SecretStatus.fromJson(<String, dynamic>{
      'set': true,
      'masked': 'sk-****cdef',
      'degraded': false,
      'broken': true,
    });
    expect(status.broken, isTrue);
    expect(status.usable, isFalse);
    // 正常状态仍可用
    expect(
      SecretStatus.fromJson(<String, dynamic>{'set': true, 'masked': 'sk-****cdef'}).usable,
      isTrue,
    );
  });

  testWidgets('凭据损坏 → 界面明确提示「已损坏」，且不显示成就绪', (WidgetTester tester) async {
    await _pump(tester);
    expect(find.textContaining('本地凭据已损坏'), findsWidgets);
    // 行为断言：点发送不会发请求（broken 视为不可用）
    await tester.enterText(find.byType(TextField).first, '你好');
    await tester.tap(find.text('发送'), warnIfMissed: false);
    await tester.pumpAndSettle();
    expect(find.textContaining('本地凭据已损坏'), findsWidgets);
  });

  test('本地搜索 query 超过 512 字符 → 显式报错（不静默返回空）', () async {
    final service = LocalSearchService(backend: _FakeLocalSearch(), dbPath: '/tmp/r5.db');
    await expectLater(
      service.search('a' * 513),
      throwsA(isA<SearchQueryTooLong>()
          .having((SearchQueryTooLong e) => e.userHint, 'hint', contains('查询过长'))),
    );
    // 恰好 512 不报错
    expect(await service.search('a' * 512), isEmpty);
  });
}
