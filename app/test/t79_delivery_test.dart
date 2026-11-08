// t79 回归：首字慢**不允许**取消流；回调先于 handle 返回也不能丢。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/secret_service.dart';

class _RecordingBackend implements ChatBackend {
  _RecordingBackend({this.emitOnStart = false});

  final bool emitOnStart;
  int cancelCalls = 0;
  int startCalls = 0;

  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) {
    startCalls += 1;
    if (emitOnStart) {
      // 模拟“Rust 在 handle 返回前就 emit”（t79 竞态场景）
      onChunk('{"type":"delta","text":"你"}');
      onChunk('{"type":"done","finish":"stop"}');
    }
    return 7;
  }

  @override
  void cancel(int handle) {
    cancelCalls += 1;
  }
}

class _KeySetSecrets implements SecretBackend {
  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef'}};

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef'}};

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
}

Future<void> _pump(WidgetTester tester, ChatBackend backend) async {
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: backend),
        secretService: SecretService(backend: _KeySetSecrets(), dbPath: '/tmp/t79.db'),
        dbPath: '/tmp/t79.db',
      ),
    ),
  ));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('首字 5 秒未到：只提示，**不取消流**（t79 根因回归）', (WidgetTester tester) async {
    final backend = _RecordingBackend();
    await _pump(tester, backend);
    await tester.enterText(find.byType(TextField).first, '你好');
    await tester.tap(find.byKey(const Key('send-button')));
    await tester.pump();

    // 跨过 5s 看门狗
    await tester.pump(const Duration(seconds: 6));
    expect(backend.startCalls, 1);
    expect(backend.cancelCalls, 0, reason: '首字慢不是错误，绝不能替用户取消流');
    expect(find.textContaining('5 秒内还没收到任何数据'), findsOneWidget);
    expect(find.textContaining('handle=7'), findsOneWidget);
  });

  testWidgets('回调早于 handle 返回：事件仍然送达（不再丢首批帧）', (WidgetTester tester) async {
    final backend = _RecordingBackend(emitOnStart: true);
    await _pump(tester, backend);
    await tester.enterText(find.byType(TextField).first, '你好');
    await tester.tap(find.byKey(const Key('send-button')));
    await tester.pumpAndSettle();

    // 首帧文本必须出现在气泡里（此前会被丢弃 → 界面空白）
    expect(find.textContaining('你'), findsWidgets);
    expect(find.textContaining('5 秒内还没收到任何数据'), findsNothing);
  });
}
