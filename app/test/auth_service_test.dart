// t60 套餐通道：登录 / 刷新 / 登出 / 失效处理（fake API + fake vault，离线）。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/auth_service.dart';

/// 可编排的假认证 API。
class FakeAuthApi implements AuthApi {
  FakeAuthApi({this.loginError, this.refreshError});

  AuthException? loginError;
  AuthException? refreshError;
  int logins = 0;
  int refreshes = 0;
  int logouts = 0;
  String lastAccess = '';
  String? lastRefreshUsed;

  @override
  Future<AuthTokens> login({required String email, required String password}) async {
    logins += 1;
    final error = loginError;
    if (error != null) {
      throw error;
    }
    lastAccess = 'access-1';
    return AuthTokens(accessToken: lastAccess, refreshToken: 'refresh-1');
  }

  @override
  Future<AuthTokens> refresh({required String refreshToken}) async {
    refreshes += 1;
    lastRefreshUsed = refreshToken;
    final error = refreshError;
    if (error != null) {
      throw error;
    }
    lastAccess = 'access-2';
    return AuthTokens(accessToken: lastAccess, refreshToken: 'refresh-2');
  }

  @override
  Future<void> logout({required String accessToken}) async {
    logouts += 1;
  }
}

void main() {
  test('登录成功后持有令牌，邮箱打码', () async {
    final api = FakeAuthApi();
    final auth = AuthService(api: api, vault: MemoryTokenVault());
    await auth.signIn(email: 'test@example.com', password: 'pw');
    expect(auth.signedIn, isTrue);
    expect(api.logins, 1);
    expect(auth.maskedEmail, 't***@example.com');
  });

  test('密码错 → 可读文案（不暴露原始错误）', () async {
    final api = FakeAuthApi(
      loginError: const AuthException('WRONG_CREDENTIALS', '邮箱或密码不正确'),
    );
    final auth = AuthService(api: api, vault: MemoryTokenVault());
    await expectLater(
      auth.signIn(email: 'test@example.com', password: 'bad'),
      throwsA(isA<AuthException>().having((AuthException e) => e.userHint, 'hint', '邮箱或密码不正确')),
    );
    expect(auth.signedIn, isFalse);
  });

  test('网络失败 → 网络文案', () async {
    final api = FakeAuthApi(
      loginError: const AuthException('NETWORK', '网络不可达，请检查网络后重试'),
    );
    final auth = AuthService(api: api, vault: MemoryTokenVault());
    await expectLater(
      auth.signIn(email: 'test@example.com', password: 'pw'),
      throwsA(isA<AuthException>().having((AuthException e) => e.code, 'code', 'NETWORK')),
    );
  });

  test('refresh 成功 → 请求带新 token', () async {
    final api = FakeAuthApi();
    final auth = AuthService(api: api, vault: MemoryTokenVault());
    await auth.signIn(email: 'test@example.com', password: 'pw');
    final token = await auth.accessToken(forceRefresh: true);
    expect(token, 'access-2');
    expect(api.refreshes, 1);
    expect(api.lastRefreshUsed, 'refresh-1');
  });

  test('refresh 失效 → 清本地 + 未登录 + 重新登录文案', () async {
    final api = FakeAuthApi();
    final vault = MemoryTokenVault();
    final auth = AuthService(api: api, vault: vault);
    await auth.signIn(email: 'test@example.com', password: 'pw');
    api.refreshError = const AuthException('UNAUTHORIZED', 'expired');

    await expectLater(
      auth.refreshTokens(),
      throwsA(isA<AuthException>()
          .having((AuthException e) => e.code, 'code', 'UNAUTHORIZED')
          .having((AuthException e) => e.userHint, 'hint', contains('重新登录'))),
    );
    expect(auth.signedIn, isFalse, reason: '失效后必须回到未登录态，不能假装已登录');
    expect(await vault.read('auth_refresh_token'), isNull, reason: '本地令牌必须清掉');
  });

  test('登出 → 调服务端吊销 + 清本地，之后取 token 为 null', () async {
    final api = FakeAuthApi();
    final vault = MemoryTokenVault();
    final auth = AuthService(api: api, vault: vault);
    await auth.signIn(email: 'test@example.com', password: 'pw');
    await auth.signOut();

    expect(api.logouts, 1);
    expect(auth.signedIn, isFalse);
    expect(await vault.read('auth_access_token'), isNull);
    expect(await auth.accessToken(), isNull, reason: '登出后不应再拿到 token');
  });

  test('恢复会话：有令牌 + refresh 成功 → 已登录；refresh 失败 → 未登录', () async {
    final api = FakeAuthApi();
    final vault = MemoryTokenVault();
    await vault.write('auth_access_token', 'access-1');
    await vault.write('auth_refresh_token', 'refresh-1');
    await vault.write('auth_email', 'test@example.com');

    final auth = AuthService(api: api, vault: vault);
    expect(await auth.restore(), isTrue);
    expect(auth.signedIn, isTrue);

    final api2 = FakeAuthApi(refreshError: const AuthException('UNAUTHORIZED', 'expired'));
    final vault2 = MemoryTokenVault();
    await vault2.write('auth_access_token', 'access-1');
    await vault2.write('auth_refresh_token', 'refresh-1');
    final auth2 = AuthService(api: api2, vault: vault2);
    expect(await auth2.restore(), isFalse);
    expect(auth2.signedIn, isFalse);
    expect(await vault2.read('auth_refresh_token'), isNull);
  });

  test('邮箱打码：test@example.com → t***@example.com', () {
    expect(AuthService.maskEmailStatic('test@example.com'), 't***@example.com');
    expect(AuthService.maskEmailStatic('no-at-sign'), 'no-at-sign');
  });
}
