// t70 UI 修复回归：主题持久化 / 诊断常驻 / 本地资料关闭 / 弹窗实心。

import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/secret_service.dart';
import 'package:yunong/src/theme/file_settings_store.dart';
import 'package:yunong/src/theme/theme_controller.dart';
import 'package:yunong/src/theme/themes.dart';

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
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

class _NoopSecrets implements SecretBackend {
  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'set': true, 'masked': 'sk-****cdef'}};

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'set': false, 'masked': ''}};

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': true}};
}

Future<void> _pumpChat(WidgetTester tester, {ChatBackend? backend, bool keySet = false}) async {
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: backend ?? _NoopChatBackend()),
        secretService: SecretService(backend: keySet ? _KeySetSecrets() : _NoopSecrets(), dbPath: '/tmp/t70.db'),
        dbPath: '/tmp/t70.db',
      ),
    ),
  ));
  await tester.pumpAndSettle();
}

void main() {
  group('P0-3 主题持久化（文件存储）', () {

    test('文件损坏不影响启动（按默认值）', () async {
      final dir = Directory.systemTemp.createTempSync('t70-settings-bad');
      final path = '${dir.path}/settings.json';
      File(path).writeAsStringSync('{not json');
      addTearDown(() => dir.deleteSync(recursive: true));

      final store = FileSettingsStore(path: path);
      expect(await store.get(SettingsKeys.themeId), isNull);
      final controller = ThemeController(store: store);
      await controller.load();
      expect(controller.themeId, AppThemeId.minimalBlack);
    });
  });

  group('P0-2 诊断入口常驻', () {
    testWidgets('菜单里始终有「对话诊断」，点开能看到 delta/事件统计', (WidgetTester tester) async {
      await _pumpChat(tester);
      await tester.tap(find.byIcon(Icons.more_vert));
      await tester.pumpAndSettle();
      expect(find.text('对话诊断'), findsOneWidget);

      await tester.tap(find.text('对话诊断'));
      await tester.pumpAndSettle();
      expect(find.text('还没有发起过对话'), findsOneWidget);
      expect(find.text('复制诊断'), findsOneWidget);
      await tester.tap(find.text('关闭'));
      await tester.pumpAndSettle();
    });

  });

  group('P1-5 本地资料面板可显式关闭', () {
  });

  group('P1-4 所有弹窗实心背景', () {
    test('三套主题的 dialogTheme 都是不透明 canvas 且无 surfaceTint', () {
      for (final id in AppThemeId.values) {
        final theme = buildTheme(id, Brightness.light);
        expect(theme.dialogTheme.backgroundColor, isNotNull, reason: '$id 对话框必须有实心背景');
        expect(theme.dialogTheme.backgroundColor!.a, 1.0, reason: '$id 对话框不能半透明');
        expect(theme.dialogTheme.surfaceTintColor, Colors.transparent);
      }
    });
  });
}

/// 模拟 owner 观察到的"三个字就停"：3 条 delta，然后**没有 done**。
class _PartialStreamBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) {
    onChunk('{"type":"delta","text":"你"}');
    onChunk('{"type":"delta","text":"可"}');
    onChunk('{"type":"delta","text":"以"}');
    return 1;
  }

  @override
  void cancel(int handle) {}
}
