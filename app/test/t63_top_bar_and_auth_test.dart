// t63：窄窗不溢出（480×900 / 800×600）+ auth_refresh 分级语义。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/auth_service.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/secret_service.dart';

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
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

class _FakeRefreshBackend implements AuthRefreshBackend {
  _FakeRefreshBackend(this.response, {this.unavailable = false});

  final Map<String, dynamic> response;
  final bool unavailable;
  int calls = 0;

  @override
  Future<Map<String, dynamic>> refreshFromStore() async {
    calls += 1;
    if (unavailable) {
      throw const AuthRefreshUnavailable('symbol missing');
    }
    return response;
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

Future<void> _pump(WidgetTester tester, Size size, AuthService auth) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  await tester.pumpWidget(MaterialApp(
    home: Scaffold(
      body: ChatPage(
        service: ChatService(backend: _NoopChatBackend()),
        secretService: SecretService(backend: _NoopSecrets(), dbPath: '/tmp/t63.db'),
        authService: auth,
        dbPath: '/tmp/t63.db',
      ),
    ),
  ));
  await tester.pumpAndSettle();
}

AuthService _auth({AuthRefreshBackend? backend}) => AuthService(
      api: _FakeAuthApi(),
      vault: MemoryTokenVault(),
      refreshBackend: backend,
    );

void main() {
  for (final size in <Size>[const Size(480, 900), const Size(800, 600)]) {
    testWidgets('窄窗 ${size.width.toInt()}x${size.height.toInt()}：对话页不溢出',
        (WidgetTester tester) async {
      await _pump(tester, size, _auth());
      expect(tester.takeException(), isNull);
    });
  }

  testWidgets('窄窗 480：打开溢出菜单也不溢出', (WidgetTester tester) async {
    await _pump(tester, const Size(480, 900), _auth());
    await tester.tap(find.byIcon(Icons.more_vert));
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
    expect(find.text('登录套餐账号'), findsWidgets);
  });

  test('auth_refresh: ok → 已登录，只用 Rust 结果', () async {
    final backend = _FakeRefreshBackend(<String, dynamic>{
      'ok': true,
      'data': <String, dynamic>{'access_token': 'access-rust', 'rotated': true},
    });
    final auth = _auth(backend: backend);
    expect(await auth.restore(), isTrue);
    expect(auth.signedIn, isTrue);
    expect(backend.calls, 1);
  });

  test('auth_refresh: UNAUTHORIZED → 未登录（清本地）', () async {
    final backend = _FakeRefreshBackend(<String, dynamic>{
      'ok': false,
      'code': 'UNAUTHORIZED',
      'message': '登录已失效',
    });
    final auth = _auth(backend: backend);
    expect(await auth.restore(), isFalse);
    expect(auth.signedIn, isFalse);
    expect(auth.lastIssue, contains('重新登录'));
  });

  test('auth_refresh: NETWORK → **不登出**，只提示稍后重试', () async {
    final vault = MemoryTokenVault();
    await vault.write('auth_access_token', 'access-1');
    final backend = _FakeRefreshBackend(<String, dynamic>{
      'ok': false,
      'code': 'NETWORK',
      'message': '网络不可用',
    });
    final auth = AuthService(api: _FakeAuthApi(), vault: vault, refreshBackend: backend);
    final restored = await auth.restore();
    expect(restored, isTrue, reason: '一次断网不能把用户登出');
    expect(await vault.read('auth_access_token'), 'access-1', reason: '令牌必须保留');
    expect(auth.lastIssue, contains('网络'));
  });

  test('auth_refresh symbol 缺失 → 降级回 vault 路径，不崩', () async {
    final backend = _FakeRefreshBackend(<String, dynamic>{}, unavailable: true);
    final vault = MemoryTokenVault();
    await vault.write('auth_access_token', 'access-1');
    await vault.write('auth_refresh_token', 'refresh-1');
    final auth = AuthService(api: _FakeAuthApi(), vault: vault, refreshBackend: backend);
    expect(await auth.restore(), isTrue);
    expect(auth.signedIn, isTrue);
    expect(backend.calls, 1);
  });
}
