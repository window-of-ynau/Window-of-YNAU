// t85：对话设置纯搬迁到 `chat_settings_section.dart` 后的冒烟测试。
// 只验证「弹窗还能打开、五个字段都在、保存不崩」——行为不变的最小证据。

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/core/secret_service.dart';
import 'package:yunong/src/settings/settings_page.dart' show openChatSettingsDialog;

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
  // t130：设置页已删掉「对话设置」入口 —— 直接打开同一个对话框，
  // 这样验证的是**面板本身**（字段齐全、保存后关闭），不依赖入口位置。
  final secrets = SecretService(backend: _Secrets(), dbPath: '/tmp/t85.db');
  final shared = ChatSettingsController();
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: Builder(
        builder: (BuildContext context) => TextButton(
          onPressed: () => openChatSettingsDialog(context, shared, secrets: secrets),
          child: const Text('open-settings'),
        ),
      ),
    ),
  ));
  await tester.pumpAndSettle();
  await tester.tap(find.text('open-settings'));
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
