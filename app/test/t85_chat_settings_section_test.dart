// t85：对话设置纯搬迁到 `chat_settings_section.dart` 后的冒烟测试。
// 只验证「弹窗还能打开、五个字段都在、保存不崩」——行为不变的最小证据。

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/secret_service.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
}

class _Secrets implements SecretBackend {
  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef'}};

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) => <String, dynamic>{
        'ok': true,
        'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef', 'degraded': false, 'broken': false},
      };

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
}

Future<void> _openSettings(WidgetTester tester) async {
  // t89：对话页已无设置入口——走真实路径：设置页 →「对话设置」
  final secrets = SecretService(backend: _Secrets(), dbPath: '/tmp/t85.db');
  final shared = ChatSettingsController();
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: _NoopChatBackend()),
        secretService: secrets,
        chatSettings: shared,
        dbPath: '/tmp/t85.db',
      ),
    ),
  ));
  await tester.pumpAndSettle();

  final NavigatorState navigator = tester.state<NavigatorState>(find.byType(Navigator));
  unawaited(navigator.push(MaterialPageRoute<void>(
    builder: (BuildContext context) => SettingsPage(
      controller: ThemeController(store: MemorySettingsStore()),
      chatSettings: shared,
      secretService: secrets,
    ),
  )));
  await tester.pumpAndSettle();
  await tester.tap(find.text('对话设置').first);
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('对话设置弹窗仍可从顶部菜单打开，且五个字段齐全', (WidgetTester tester) async {
    await _openSettings(tester);

    expect(find.widgetWithText(AlertDialog, '对话设置'), findsOneWidget);
    // 五个字段：通道 / 模型 / 协议 / BYOK API Key / 套餐访问令牌
    expect(find.text('通道'), findsWidgets);
    expect(find.text('模型'), findsWidgets);
    expect(find.text('协议'), findsWidgets);
    expect(find.text('BYOK API Key'), findsWidgets);
    expect(find.textContaining('套餐访问令牌'), findsWidgets);
    // 仍由同一个（搬迁后的）组件承载
    expect(find.byType(ChatSettingsSection), findsOneWidget);
  });

  testWidgets('保存后弹窗关闭，且不抛异常（行为不变）', (WidgetTester tester) async {
    await _openSettings(tester);
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();

    expect(tester.takeException(), isNull);
    expect(find.widgetWithText(AlertDialog, '对话设置'), findsNothing, reason: '保存后应关闭对话框');
  });
}
