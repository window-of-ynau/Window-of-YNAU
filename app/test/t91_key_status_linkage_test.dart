// t91：Key 状态联动 + “读取失败不得抹掉已知状态”的回归测试。
//
// 关键：断言直接读 `ChatPageState.keyStatusLabel` / `keyStatusError`（@visibleForTesting），
// 不走 Tooltip —— 前三次失败都是读错了 Tooltip（另一个控件或 offstage 文案）。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/secret_service.dart';

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
}

/// 可切换“正常 / 抛异常”的密文后端（模拟 DPAPI 偶发失败、DB 忙等）。
class _FlakySecrets implements SecretBackend {
  _FlakySecrets({this.stored});

  String? stored;
  bool failGet = false;

  String get _masked => 'sk-****${stored!.substring(stored!.length - 4)}';

  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) {
    stored = value;
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{'set': true, 'masked': _masked, 'degraded': false, 'broken': false},
    };
  }

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) {
    if (failGet) {
      throw StateError('DPAPI 暂时不可用（模拟）');
    }
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{
        'set': stored != null,
        'masked': stored == null ? '' : _masked,
        'degraded': false,
        'broken': false,
      },
    };
  }

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) {
    stored = null;
    return <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
  }
}

Future<ChatPageState> _pump(
  WidgetTester tester,
  ChatSettingsController shared,
  SecretService secrets,
) async {
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: _NoopChatBackend()),
        secretService: secrets,
        chatSettings: shared,
        dbPath: '/tmp/t91.db',
      ),
    ),
  ));
  await tester.pumpAndSettle();
  return tester.state<ChatPageState>(find.byType(ChatPage));
}

void main() {
  testWidgets('初始「未设置」→ 保存 Key 后 keyStatusLabel 立即更新为打码串', (WidgetTester tester) async {
    final backend = _FlakySecrets();
    final secrets = SecretService(backend: backend, dbPath: '/tmp/t91.db');
    final shared = ChatSettingsController();

    final state = await _pump(tester, shared, secrets);
    expect(state.keyStatusLabel, '未设置');
    expect(state.keyStatusError, isNull);

    // 模拟“在设置页保存了 Key”：写入安全存储 + 通知共享源
    await secrets.save(SecretService.byokKeyName, 'sk-test-7777');
    shared.apply(ChatSettings());
    await tester.pumpAndSettle();

    expect(state.keyStatusLabel, 'sk-****7777', reason: '共享源变化 → 对话页刷新 Key 状态');
    expect(state.keyStatusError, isNull);
  });

  testWidgets('读取状态抛异常时：**保留上一次的值**（不抹成未设置），并记录错误', (WidgetTester tester) async {
    final backend = _FlakySecrets();
    final secrets = SecretService(backend: backend, dbPath: '/tmp/t91.db');
    final shared = ChatSettingsController();

    final state = await _pump(tester, shared, secrets);
    await secrets.save(SecretService.byokKeyName, 'sk-test-7777');
    shared.apply(ChatSettings());
    await tester.pumpAndSettle();
    expect(state.keyStatusLabel, 'sk-****7777');

    // 现在让读取失败，再触发一次刷新
    backend.failGet = true;
    shared.apply(ChatSettings());
    await tester.pumpAndSettle();

    expect(state.keyStatusLabel, 'sk-****7777',
        reason: '读取失败不能把用户的“已配置”抹成“未设置”（旧实现就是这么干的）');
    expect(state.keyStatusError, isNotNull, reason: '失败必须可观测');
    expect(state.keyStatusError, contains('DPAPI'));
  });
}
