// t49 对话发送回归测试：通道拦截 / 未就绪禁用 / Enter 发送 / Shift+Enter / 组合输入。

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/secret_service.dart';

/// 记录是否真的被调用（未就绪时不许调用）。
class CountingChatBackend implements ChatBackend {
  int starts = 0;

  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) {
    starts += 1;
    onChunk('{"type":"delta","text":"回"}');
    onChunk('{"type":"done","finish":"stop"}');
    return 1;
  }

  @override
  void cancel(int handle) {}
}

class _Backend implements SecretBackend {
  _Backend({this.set = false});

  final bool set;
  final Map<String, String> entries = <String, String>{};

  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef'}};

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) => <String, dynamic>{
        'ok': true,
        'data': <String, dynamic>{'set': set, 'masked': set ? 'sk-****cdef' : ''},
      };

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
}

Future<CountingChatBackend> _pump(
  WidgetTester tester, {
  bool keySet = false,
  ChatSettings? settings,
}) async {
  final backend = CountingChatBackend();
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: backend),
        secretService: SecretService(backend: _Backend(set: keySet), dbPath: ''),
        settings: settings,
      ),
    ),
  ));
  await tester.pumpAndSettle();
  return backend;
}

void main() {
  testWidgets('P0-2：BYOK 未填 Key → 本地拦截，不发请求且给出可读原因', (WidgetTester tester) async {
    final backend = await _pump(tester);
    await tester.enterText(find.byType(TextField).first, '你好');
    await tester.tap(find.byKey(const Key('send-button')));
    await tester.pumpAndSettle();

    expect(backend.starts, 0, reason: '未就绪时绝不能发请求');
    expect(find.textContaining('未设置 API Key'), findsWidgets);
  });

  testWidgets('P0-4：套餐无令牌 → 发送按钮禁用并说明原因', (WidgetTester tester) async {
    final backend = await _pump(tester, settings: ChatSettings()..channel = 'cloud');
    // 行为断言：点发送 → 本地拦截，不产生任何请求；并显示未就绪原因
    await tester.enterText(find.byType(TextField).first, '你好');
    await tester.tap(find.byKey(const Key('send-button')), warnIfMissed: false);
    await tester.pumpAndSettle();
    expect(find.textContaining('请先登录套餐账号'), findsWidgets);    expect(backend.starts, 0);
  });

  testWidgets('就绪后 Enter 发送，Shift+Enter 换行', (WidgetTester tester) async {
    final backend = await _pump(tester, keySet: true);
    final field = find.byType(TextField).first;
    await tester.enterText(field, '你好');
    await tester.pumpAndSettle();

    // Shift+Enter：不发送，插入换行
    await tester.sendKeyDownEvent(LogicalKeyboardKey.shiftLeft);
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.sendKeyUpEvent(LogicalKeyboardKey.shiftLeft);
    await tester.pumpAndSettle();
    expect(backend.starts, 0, reason: 'Shift+Enter 不应发送');

    // Enter：发送
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pumpAndSettle();
    expect(backend.starts, 1, reason: 'Enter 应发送');
  });

  testWidgets('输入法组合期间 Enter 不发送', (WidgetTester tester) async {
    final backend = await _pump(tester, keySet: true);
    final field = find.byType(TextField).first;
    await tester.enterText(field, 'nihao');
    // 模拟拼音未上屏：composing 覆盖整个文本
    final controller = tester.widget<TextField>(field).controller!;
    controller.value = controller.value.copyWith(
      composing: const TextRange(start: 0, end: 5),
    );
    await tester.pumpAndSettle();

    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pumpAndSettle();
    expect(backend.starts, 0, reason: '组合输入回车是确认候选，不是发送');
  });

  testWidgets('空消息不发送', (WidgetTester tester) async {
    final backend = await _pump(tester, keySet: true);
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pumpAndSettle();
    expect(backend.starts, 0);
  });
}
