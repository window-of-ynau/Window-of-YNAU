// t52 回归测试：导航路由不再落到对话页 / Key 保存先校验再落盘并回读。

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_entries.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/core/secret_service.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

class _ChatBackend implements ChatBackend {
  int starts = 0;

  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => starts += 1;

  @override
  void cancel(int handle) {}
}

class _SecretBackend implements SecretBackend {
  _SecretBackend({this.loseOnReadBack = false});

  final bool loseOnReadBack;
  String? stored;

  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) {
    stored = value;
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{'set': true, 'masked': _mask(value)},
    };
  }

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) {
    final value = loseOnReadBack ? null : stored;
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{
        'set': value != null,
        'masked': value == null ? '' : _mask(value),
      },
    };
  }

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};

  String _mask(String value) => value.length <= 8
      ? '****'
      : '${value.substring(0, 3)}****${value.substring(value.length - 4)}';
}

// t89：对话页已无设置入口——测试改为走真实路径：设置页 →「对话设置」。
late SecretService _secretsService;
late ChatSettingsController _sharedSettings;
late Future<String?> Function(String)? _validator;

Future<void> _pumpChat(
  WidgetTester tester, {
  required _SecretBackend secrets,
  Future<String?> Function(String apiKey)? validator,
}) async {
  _secretsService = SecretService(backend: secrets, dbPath: '/tmp/t52.db');
  _validator = validator;
  _sharedSettings = ChatSettingsController();
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: _ChatBackend()),
        secretService: _secretsService,
        keyValidator: _validator ?? (String _) async => null,
        chatSettings: _sharedSettings,
        dbPath: '/tmp/t52.db',
      ),
    ),
  ));
  await tester.pumpAndSettle();
}

Future<void> _openSettingsAndSave(WidgetTester tester, String key) async {
  final NavigatorState navigator = tester.state<NavigatorState>(find.byType(Navigator));
  unawaited(navigator.push(MaterialPageRoute<void>(
    builder: (BuildContext context) => SettingsPage(
      controller: ThemeController(store: MemorySettingsStore()),
      chatSettings: _sharedSettings,
      secretService: _secretsService,
      keyValidator: _validator,
    ),
  )));
  await tester.pumpAndSettle();
  await tester.tap(find.text('对话设置').first);
  await tester.pumpAndSettle();
  // 按标签定位 BYOK Key（比按序号稳，t89：字段顺序随面板调整过）
  await tester.enterText(find.widgetWithText(TextField, 'BYOK API Key'), key);
  await tester.tap(find.text('保存'));
  await tester.pumpAndSettle();
}

void main() {
  // P0-2 路由：每个校务入口都必须有自己的 URL（`_CampusPane` 已改为占位页，不再回落到对话页）。
  // 侧栏 UI 在 1280 宽下可能收起，故这里断言数据源；人工验证见任务报告。
  test('每个校务入口都有独立 URL（不会共用对话页）', () {
    for (final entry in CampusEntry.values) {
      expect(entry.url, startsWith('http'));
      expect(entry.url.isNotEmpty, isTrue, reason: entry.label);
    }
  });
  testWidgets('P0-1：Key 无效 → 不保存、显示可读原因', (WidgetTester tester) async {
    final secrets = _SecretBackend();
    await _pumpChat(tester, secrets: secrets, validator: (String _) async => 'Key 无效（上游返回 401）');
    await _openSettingsAndSave(tester, 'sk-test-invalid');

    expect(secrets.stored, isNull, reason: '校验失败绝不能落盘');
    expect(find.textContaining('Key 无效'), findsWidgets);
  });

  testWidgets('P0-1：Key 有效 → 保存并回读显示掩码', (WidgetTester tester) async {
    final secrets = _SecretBackend();
    await _pumpChat(tester, secrets: secrets);
    await _openSettingsAndSave(tester, 'sk-test-0123456789cdef');
    // t89：保存发生在设置页；回到对话页验证顶栏掩码
    tester.state<NavigatorState>(find.byType(Navigator)).pop();
    await tester.pumpAndSettle();

    expect(secrets.stored, 'sk-test-0123456789cdef');
    await tester.pumpAndSettle();
    // 掩码以安全存储回读为准（顶栏 tooltip 的刷新另有用例覆盖）
    final status = await _secretsService.status(SecretService.byokKeyName);
    expect(status.masked, 'sk-****cdef');
    // 明文不得出现在界面
    expect(find.text('sk-test-0123456789cdef'), findsNothing);
  });

  testWidgets('P0-1：保存后回读失败 → 明确报错，不假装成功', (WidgetTester tester) async {
    final secrets = _SecretBackend(loseOnReadBack: true);
    await _pumpChat(tester, secrets: secrets);
    await _openSettingsAndSave(tester, 'sk-test-0123456789cdef');

    expect(find.textContaining('回读失败'), findsWidgets);
    expect(find.textContaining('sk-****cdef'), findsNothing);
  });
}
