// t88：账号 / Key 能力下沉到 ChatSettingsSection 的验证（入口保持原样）。

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/core/auth_service.dart';
import 'package:yunong/src/core/secret_service.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

class _Secrets implements SecretBackend {
  _Secrets();

  String? stored = 'sk-test-1234';

  @override
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath}) {
    stored = value;
    final masked = 'sk-****${value.length >= 4 ? value.substring(value.length - 4) : value}';
    return <String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{'set': true, 'masked': masked, 'degraded': false, 'broken': false},
    };
  }

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) => <String, dynamic>{
        'ok': true,
        'data': <String, dynamic>{
          'set': stored != null,
          'masked': stored == null ? '' : 'sk-****${stored!.substring(stored!.length - 4)}',
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

class _FakeAuthApi implements AuthApi {
  @override
  Future<AuthTokens> login({required String email, required String password}) async =>
      const AuthTokens(accessToken: 'a', refreshToken: 'r');

  @override
  Future<AuthTokens> refresh({required String refreshToken}) async =>
      const AuthTokens(accessToken: 'a2', refreshToken: 'r2');

  @override
  Future<void> logout({required String accessToken}) async {}
}

void main() {
  test('保存 Key 的校验/回读逻辑可独立测试（不依赖 ChatPage）', () async {
    final secrets = SecretService(backend: _Secrets(), dbPath: '/tmp/t88.db');
    final status = await saveKeyWithService(
      secrets: secrets,
      apiKey: 'sk-test-abcd',
      validate: (String key) async => key.startsWith('sk-') ? null : '格式不对',
    );
    expect(status.set, isTrue);
    expect(status.masked, 'sk-****abcd');

    // 校验失败 → 抛可读异常，且不会写坏存储
    await expectLater(
      saveKeyWithService(secrets: secrets, apiKey: 'bad', validate: (String key) async => '格式不对'),
      throwsA(isA<ChatKeySaveException>().having((ChatKeySaveException e) => e.message, 'message', '格式不对')),
    );
  });

  testWidgets('设置页不再有「对话设置」入口；面板本身账号能力仍可用（t130）', (WidgetTester tester) async {
    final theme = ThemeController(store: MemorySettingsStore());
    final chat = ChatSettingsController();
    final auth = AuthService(api: _FakeAuthApi(), vault: MemoryTokenVault());

    await tester.pumpWidget(MaterialApp(
      home: SettingsPage(controller: theme, chatSettings: chat, authService: auth),
    ));
    await tester.pumpAndSettle();

    // t130：设置页已删掉「对话设置」入口（IA 合并到供应商）
    expect(find.text('对话设置'), findsNothing);

    // 面板能力本身仍在：直接打开同一个对话框验证（不依赖入口位置）
    final BuildContext context = tester.element(find.byType(SettingsPage));
    unawaited(openChatSettingsDialog(context, chat, auth: auth));
    await tester.pumpAndSettle();

    // 账号行存在且可点「登录」（能力已下沉：不依赖对话页）
    expect(find.textContaining('套餐账号：未登录'), findsOneWidget);
      await tester.tap(find.widgetWithText(TextButton, '登录').last);
    await tester.pumpAndSettle();
    expect(find.widgetWithText(TextField, '邮箱'), findsOneWidget);
    expect(find.widgetWithText(TextField, '密码'), findsOneWidget);
  });

}
