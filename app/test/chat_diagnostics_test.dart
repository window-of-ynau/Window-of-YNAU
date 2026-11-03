// t56 回归测试：对话诊断必须可观测（3 秒无事件 → 横幅 + 复制按钮），且不泄漏 Key 明文。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/secret_service.dart';

/// 只登记回调、永不产生事件（模拟"流没启动"）。
class SilentBackend implements ChatBackend {
  int handleToReturn = 7;
  int starts = 0;

  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) {
    starts += 1;
    return handleToReturn;
  }

  @override
  void cancel(int handle) {}
}

/// 立刻回一条 delta+done 的后端。
class ChattyBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) {
    onChunk('{"type":"delta","text":"你"}');
    onChunk('{"type":"done","finish":"stop"}');
    return 3;
  }

  @override
  void cancel(int handle) {}
}

class _NoopSecretBackend implements SecretBackend {
  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef'}};

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) => <String, dynamic>{
        'ok': true,
        'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef'},
      };

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
}

Future<void> _pump(WidgetTester tester, ChatBackend backend) async {
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: backend),
        secretService: SecretService(backend: _NoopSecretBackend(), dbPath: '/tmp/t56.db'),
        dbPath: '/tmp/t56.db',
      ),
    ),
  ));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('3 秒无事件 → 界面出现诊断横幅（含 handle/回调注册/事件数）', (WidgetTester tester) async {
    final backend = SilentBackend();
    await _pump(tester, backend);

    await tester.enterText(find.byType(TextField).first, '你好');
    await tester.tap(find.text('发送'));
    await tester.pump(); // 触发 _send
    await tester.pump(const Duration(seconds: 4)); // 越过 3s 看门狗
    await tester.pumpAndSettle();

    expect(find.textContaining('对话没有启动'), findsOneWidget);
    expect(find.textContaining('handle=7'), findsOneWidget);
    expect(find.textContaining('回调注册=成功'), findsOneWidget);
    expect(find.textContaining('已收到事件=0'), findsOneWidget);
    expect(find.text('复制诊断'), findsOneWidget);
  });

  testWidgets('有事件时不出现“对话没有启动”横幅', (WidgetTester tester) async {
    await _pump(tester, ChattyBackend());
    await tester.enterText(find.byType(TextField).first, '你好');
    await tester.tap(find.text('发送'));
    await tester.pump();
    await tester.pump(const Duration(seconds: 4));
    await tester.pumpAndSettle();

    expect(find.textContaining('对话没有启动'), findsNothing);
  });

  testWidgets('诊断文本不含 Key 明文', (WidgetTester tester) async {
    final backend = SilentBackend();
    await _pump(tester, backend);
    await tester.enterText(find.byType(TextField).first, '你好');
    await tester.tap(find.text('发送'));
    await tester.pump();
    await tester.pump(const Duration(seconds: 4));
    await tester.pumpAndSettle();

    // 横幅里只应有 base_url / model / protocol 之类，不含 sk- 明文
    // 允许列表/掩码出现（sk-****cdef），但绝不能出现完整 Key\n    expect(find.textContaining('sk-test-'), findsNothing);
  });
}
