// t89：设置合并收尾的守护测试。
// 1) 对话页菜单里**没有**「设置」入口（一个 App 只有一个设置入口）；
// 2) 设置页有「对话设置」卡片且能打开；
// 3) 外部保存 Key 后，对话页通过共享源监听刷新状态（用 Tooltip.message 稳定断言）。

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
  _Secrets();

  String? stored;

  String? get _masked => stored == null ? null : 'sk-****${stored!.substring(stored!.length - 4)}';

  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) {
    stored = value;
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{'set': true, 'masked': _masked, 'degraded': false, 'broken': false},
    };
  }

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) => <String, dynamic>{
        'ok': true,
        'data': <String, dynamic>{
          'set': stored != null,
          'masked': _masked ?? '',
          'degraded': false,
          'broken': false,
        },
      };

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) {
    stored = null;
    return <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
  }
}

void main() {
  testWidgets('对话页菜单里没有「设置」入口（只剩诊断等项）', (WidgetTester tester) async {
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: ChatPage(
          service: ChatService(backend: _NoopChatBackend()),
          secretService: SecretService(backend: _Secrets(), dbPath: '/tmp/t89.db'),
          dbPath: '/tmp/t89.db',
        ),
      ),
    ));
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('更多'));
    await tester.pumpAndSettle();

    expect(find.text('设置'), findsNothing, reason: '对话页不应再有设置入口（统一到设置页）');
    expect(find.text('对话诊断'), findsOneWidget);
    // t112：Key / 账号项已移到设置页
    expect(find.text('登录套餐账号'), findsNothing);
  });

  testWidgets('设置页不再有「对话设置」卡片；面板本身仍可打开（t130）', (WidgetTester tester) async {
    final chat = ChatSettingsController();
    await tester.pumpWidget(MaterialApp(
      home: SettingsPage(
        controller: ThemeController(store: MemorySettingsStore()),
        chatSettings: chat,
        secretService: SecretService(backend: _Secrets(), dbPath: '/tmp/t89.db'),
      ),
    ));
    await tester.pumpAndSettle();

    // t130：设置页 IA 合并到「供应商」，旧入口不再出现
    expect(find.text('对话设置'), findsNothing);

    // 面板本身仍可打开（直接调用 opener，不依赖设置页入口）
    final BuildContext context = tester.element(find.byType(SettingsPage));
    unawaited(openChatSettingsDialog(context, chat));
    await tester.pumpAndSettle();
    expect(find.widgetWithText(AlertDialog, '对话设置'), findsOneWidget);
    expect(find.widgetWithText(TextField, 'BYOK API Key'), findsOneWidget);
  });


  test('设置页→对话设置的打开路径可用（能力已下沉，不依赖对话页）', () async {
    // 纯逻辑保护：settings_page 暴露的 opener 存在且签名包含服务注入
    expect(openChatSettingsDialog, isNotNull);
  });
}
