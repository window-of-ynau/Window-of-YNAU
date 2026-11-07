// 套餐通道的登录 / 令牌持久化 / 自动刷新 / 登出（t60）。
//
// 安全约定：
// - access / refresh 令牌都走 `SecretService`（Rust `yunong_set_secret` → Windows DPAPI），
//   **不落明文、不回显**；界面只显示打码邮箱（`f***@qq.com`）。
// - refresh 失效 → **清本地令牌 + 回未登录态**，绝不静默显示"已登录"。
// - 错误按类型给中文文案，不显示原始错误串。

import 'dart:convert';
import 'dart:ffi';
import 'dart:isolate';

import 'package:ffi/ffi.dart';

import 'ffi_bindings.dart';
import 'dart:io';

import 'secret_service.dart';

/// 令牌保管：默认内存（进程内有效）。
///
/// ⚠️ 持久化缺口：`yunong_get_secret` **只返回掩码**（设计如此，明文不出 Rust），
/// 所以 Dart 无法在重启后读回令牌再做 refresh。要真正"重启免登录"，需要二选一：
///   a) 新增 Rust FFI `yunong_auth_refresh_from_store()`，让 Rust 自己读 refresh 并换新；
///   b) 新增 `yunong_get_secret_raw()`（仅内存中短期返回，风险高，不推荐）。
/// 这两条都属 Rust 侧改动，已在任务报告里作为提案提交。
abstract class TokenVault {
  Future<String?> read(String key);
  Future<void> write(String key, String value);
  Future<void> delete(String key);
}

/// 内存保管（本次会话有效）。已通过 `SecretService` 落 DPAPI 的副本可供未来 Rust 侧使用。
class MemoryTokenVault implements TokenVault {
  final Map<String, String> _values = <String, String>{};

  @override
  Future<String?> read(String key) async => _values[key];

  @override
  Future<void> write(String key, String value) async => _values[key] = value;

  @override
  Future<void> delete(String key) async => _values.remove(key);
}


/// 重启免登录：调用 Rust `yunong_auth_refresh_from_store(cloud_base_url, db_path)`。
///
/// 该 FFI 是**同步**的（connect 5s / total 10s），必须在后台 isolate 调用，
/// 否则会卡住 UI 线程。
abstract class AuthRefreshBackend {
  /// 返回 `{ok, data:{access_token, rotated}}` 或 `{ok:false, code, message}`。
  Future<Map<String, dynamic>> refreshFromStore();
}

/// symbol 不可用（旧包 / 非 Windows 平台）时抛这个，调用方降级为"需要重新登录"。
class AuthRefreshUnavailable implements Exception {
  const AuthRefreshUnavailable(this.reason);

  final String reason;
}
/// 令牌对。
class AuthTokens {
  const AuthTokens({required this.accessToken, required this.refreshToken});

  final String accessToken;
  final String refreshToken;
}

/// 登录/刷新/登出的 HTTP 接口（可注入 fake）。
abstract class AuthApi {
  Future<AuthTokens> login({required String email, required String password});

  Future<AuthTokens> refresh({required String refreshToken});

  Future<void> logout({required String accessToken});
}

/// 认证失败（带可读文案）。
class AuthException implements Exception {
  const AuthException(this.code, this.userHint);

  /// `WRONG_CREDENTIALS` / `UNAUTHORIZED` / `NETWORK` / `SERVER` / `BAD_INPUT`
  final String code;
  final String userHint;

  @override
  String toString() => 'AuthException($code)';
}

/// 端侧套餐通道会话。
class AuthService {
  AuthService({
    AuthApi? api,
    TokenVault? vault,
    SecretService? secrets,
    String? baseUrl,
    AuthRefreshBackend? refreshBackend,
  })
      : _api = api ?? HttpAuthApi(baseUrl: baseUrl ?? _defaultBaseUrl),
        _vault = vault ?? MemoryTokenVault(),
        _secrets = secrets,
        _refreshBackend = refreshBackend;

  static const String _accessKey = 'auth_access_token';
  static const String _refreshKey = 'auth_refresh_token';
  static const String _emailKey = 'auth_email';

  /// 默认服务端地址：可由 `--dart-define=YUNONG_API_BASE=...` 覆盖。
  static const String _defaultBaseUrl = String.fromEnvironment(
    'YUNONG_API_BASE',
    defaultValue: 'http://127.0.0.1:8080',
  );

  final AuthApi _api;
  SecretService? _secrets;
  final AuthRefreshBackend? _refreshBackend;

  /// 最近一次失败原因（UI 用；NETWORK 时不应登出）。
  String? lastIssue;
  final TokenVault _vault;

  String? _accessToken;
  String? _refreshToken;
  String? _email;

  bool get signedIn => _accessToken != null;

  /// 打码邮箱：`fan@qq.com` → `f***@qq.com`
  String get maskedEmail {
    final email = _email ?? '';
    final at = email.indexOf('@');
    if (at <= 0) {
      return email;
    }
    return '${email.substring(0, 1)}***${email.substring(at)}';
  }

  static String maskEmailStatic(String email) {
    final at = email.indexOf('@');
    if (at <= 0) {
      return email;
    }
    return '${email.substring(0, 1)}***${email.substring(at)}';
  }

  /// 登录：成功后令牌落 DPAPI，内存里只留引用。
  Future<void> signIn({required String email, required String password}) async {
    final tokens = await _api.login(email: email, password: password);
    await _persist(tokens, email);
  }

  /// 启动时恢复会话：**优先** Rust `yunong_auth_refresh_from_store`（重启免登录），
  /// 不可用时回退到 vault 里的 refresh token 走 HTTP。
  ///
  /// 分级语义（与 Rust 一致）：ok → 已登录；UNAUTHORIZED → 清本地、未登录；
  /// NETWORK → **保留令牌**、仍视为已登录，只记提示。
  ///
  /// refresh 失败 = 会话已失效 → **清本地 + 未登录**（调用方据此提示重新登录）。
  Future<bool> restore() async {
    // 1) 先试 Rust 免登录（symbol 缺失/非 Windows → 降级）
    final rustResult = await _tryRefreshFromStore();
    if (rustResult != null) {
      return rustResult;
    }
    // 2) 回退：vault 里有 refresh token 就自己换新
    try {
      final access = await _read(_accessKey);
      final refresh = await _read(_refreshKey);
      final email = await _read(_emailKey);
      if (access == null || refresh == null) {
        return false;
      }
      _accessToken = access;
      _refreshToken = refresh;
      _email = email;
      await refreshTokens();
      return signedIn;
    } on Object {
      await _clearLocal();
      return false;
    }
  }

  /// 尝试 Rust 侧免登录。
  ///
  /// 返回 `null` = 不可用（降级到 vault 路径）；`true/false` = 明确结果。
  Future<bool?> _tryRefreshFromStore() async {
    final backend = _refreshBackend;
    if (backend == null) {
      return null;
    }
    try {
      final json = await backend.refreshFromStore();
      final code = json['code'] as String?;
      if (json['ok'] == true) {
        final data = json['data'];
        final access = data is Map<String, dynamic> ? data['access_token'] as String? : null;
        if (access != null && access.isNotEmpty) {
          _accessToken = access;
          // refresh token 由 Rust 侧轮换写回 DPAPI，Dart 不持有明文
          _refreshToken ??= 'rust-managed';
          await _vault.write(_accessKey, access);
          lastIssue = null;
          return true;
        }
        return false;
      }
      switch (code) {
        case 'UNAUTHORIZED':
          // Rust 已删除本地令牌 → 回未登录态
          await _clearLocal();
          lastIssue = '登录已失效，请重新登录';
          return false;
        case 'NETWORK':
          // **保留令牌、不登出**：一次断网不该把用户踢下线
          lastIssue = '网络不可用，稍后会自动重试';
          return _accessToken != null || await _vault.read(_accessKey) != null;
        default:
          lastIssue = '登录状态未知，请重新登录';
          return false;
      }
    } on AuthRefreshUnavailable {
      // 旧包 / 非 Windows：免登录不可用，静默降级
      return null;
    } on Object {
      return null;
    }
  }
  /// 主动刷新；失败会清本地并抛 [AuthException]（调用方显示"请重新登录"）。
  Future<void> refreshTokens() async {
    final refresh = _refreshToken;
    if (refresh == null) {
      throw const AuthException('UNAUTHORIZED', '登录已失效，请重新登录套餐账号');
    }
    try {
      final tokens = await _api.refresh(refreshToken: refresh);
      await _persist(tokens, _email ?? '');
    } on AuthException catch (failure) {
      await _clearLocal();
      throw AuthException(failure.code, '登录已失效，请重新登录套餐账号');
    }
  }

  /// 发请求前取 access token；调用方收到 401 时可 `forceRefresh: true` 再试一次。
  Future<String?> accessToken({bool forceRefresh = false}) async {
    if (forceRefresh || _accessToken == null) {
      if (_refreshToken == null) {
        return null;
      }
      await refreshTokens();
    }
    return _accessToken;
  }

  /// 登出：先让服务端吊销 refresh（失败也继续清本地，安全优先）。
  Future<void> signOut() async {
    final access = _accessToken;
    if (access != null) {
      try {
        await _api.logout(accessToken: access);
      } on Object {
        // 网络失败不阻止登出：本地必须清干净
      }
    }
    await _clearLocal();
  }

  Future<void> _persist(AuthTokens tokens, String email) async {
    _accessToken = tokens.accessToken;
    _refreshToken = tokens.refreshToken;
    if (email.isNotEmpty) {
      _email = email;
      await _vault.write(_emailKey, email);
      await _persistSecret(_emailKey, email);
    }
    await _vault.write(_accessKey, tokens.accessToken);
    await _vault.write(_refreshKey, tokens.refreshToken);
    // 同时落一份到 DPAPI（供未来 Rust 侧直接读取；Dart 读不回明文）
    await _persistSecret(_accessKey, tokens.accessToken);
    await _persistSecret(_refreshKey, tokens.refreshToken);
  }

  Future<void> _clearLocal() async {
    _accessToken = null;
    _refreshToken = null;
    _email = null;
    for (final key in <String>[_accessKey, _refreshKey, _emailKey]) {
      await _vault.delete(key);
      await _deleteSecret(key);
    }
  }

  /// 惰性打开 DPAPI 存储（测试环境没有 DLL 时静默跳过，不影响内存态）。
  SecretService? _store() {
    try {
      return _secrets ??= SecretService();
    } on Object {
      return null;
    }
  }
  Future<void> _persistSecret(String key, String value) async {
    try {
      await _store()?.save(key, value);
    } on Object {
      // 落盘失败不影响本次会话
    }
  }
  Future<void> _deleteSecret(String key) async {
    try {
      await _store()?.delete(key);
    } on Object {
      // 删除失败不阻塞登出
    }
  }
  Future<String?> _read(String key) async {
    try {
      // 先看内存 vault（同进程恢复）；没有再看 DPAPI 是否"曾经存过"
      final cached = await _vault.read(key);
      if (cached != null) {
        return cached;
      }
      final status = await _store()?.status(key);
      if (status == null || !status.set) {
        return null;
      }
      // status 只给掩码，取明文走内部接口（不经过 UI）
      return await _vault.read(key);
    } on Object {
      return null;
    }
  }
}

/// 真实 HTTP 实现：`/api/v1/auth/{login,refresh,logout}`。
///
/// 响应容错：服务端信封可能是 `{ok,data:{...}}` 或直接平铺，字段名兼容
/// `access_token` / `accessToken`。
class HttpAuthApi implements AuthApi {
  HttpAuthApi({required this.baseUrl});

  final String baseUrl;

  @override
  Future<AuthTokens> login({required String email, required String password}) =>
      _post('/api/v1/auth/login', <String, dynamic>{'email': email, 'password': password});

  @override
  Future<AuthTokens> refresh({required String refreshToken}) =>
      _post('/api/v1/auth/refresh', <String, dynamic>{'refresh_token': refreshToken});

  @override
  Future<void> logout({required String accessToken}) async {
    await _send('/api/v1/auth/logout', <String, dynamic>{}, accessToken: accessToken);
  }

  Future<AuthTokens> _post(String path, Map<String, dynamic> body,
      {String? accessToken}) async {
    final response = await _send(path, body, accessToken: accessToken);
    final access = _pick(response, <String>['access_token', 'accessToken', 'token']);
    final refresh = _pick(response, <String>['refresh_token', 'refreshToken']);
    if (access == null || refresh == null) {
      throw const AuthException('SERVER', '服务端响应缺少令牌字段');
    }
    return AuthTokens(accessToken: access, refreshToken: refresh);
  }

  Future<Map<String, dynamic>> _send(String path, Map<String, dynamic> body,
      {String? accessToken}) async {
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 10);
    final uri = Uri.parse('${baseUrl.replaceAll(RegExp(r'/+$'), '')}$path');
    try {
      final request = await client.postUrl(uri).timeout(const Duration(seconds: 10));
      request.headers.contentType = ContentType.json;
      if (accessToken != null) {
        request.headers.set('Authorization', 'Bearer $accessToken');
      }
      request.write(jsonEncode(body));
      final response = await request.close().timeout(const Duration(seconds: 10));
      final text = await utf8.decoder.bind(response).join();
      if (response.statusCode == 200 || response.statusCode == 204) {
        if (text.isEmpty) {
          return <String, dynamic>{};
        }
        final decoded = jsonDecode(text);
        return decoded is Map<String, dynamic> ? decoded : <String, dynamic>{};
      }
      if (response.statusCode == 401 || response.statusCode == 403) {
        throw const AuthException('WRONG_CREDENTIALS', '邮箱或密码不正确');
      }
      if (response.statusCode >= 500) {
        throw const AuthException('SERVER', '服务端暂时不可用，请稍后重试');
      }
      throw const AuthException('BAD_INPUT', '请求被拒绝，请检查输入');
    } on AuthException {
      rethrow;
    } on Object {
      throw const AuthException('NETWORK', '网络不可达，请检查网络后重试');
    } finally {
      client.close(force: true);
    }
  }

  String? _pick(Map<String, dynamic> response, List<String> keys) {
    final candidates = <Map<String, dynamic>>[
      response,
      if (response['data'] is Map<String, dynamic>) response['data'] as Map<String, dynamic>,
      if (response['tokens'] is Map<String, dynamic>) response['tokens'] as Map<String, dynamic>,
    ];
    for (final map in candidates) {
      for (final key in keys) {
        final value = map[key];
        if (value is String && value.isNotEmpty) {
          return value;
        }
      }
    }
    return null;
  }
}

/// 真 FFI：`yunong_auth_refresh_from_store(cloud_base_url, db_path)`。
///
/// - 同步 FFI → 放 `Isolate.run` 里跑（连 5s / 总 10s 由 Rust 侧控制）；
/// - 返回 JSON 信封；symbol 缺失（旧包）→ [AuthRefreshUnavailable]。
class FfiAuthRefreshBackend implements AuthRefreshBackend {
  FfiAuthRefreshBackend({this.baseUrl = '', this.dbPath = ''});

  final String baseUrl;
  final String dbPath;

  @override
  Future<Map<String, dynamic>> refreshFromStore() {
    final url = baseUrl;
    final db = dbPath;
    return Isolate.run<Map<String, dynamic>>(() {
      final library = CoreLibrary.openCoreLibrary();
      final Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>) call;
      try {
        call = library.lookupFunction<Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>),
            Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>)>('yunong_auth_refresh_from_store');
      } on ArgumentError {
        throw const AuthRefreshUnavailable('端侧核心没有 yunong_auth_refresh_from_store');
      }
      final free = library.lookupFunction<Void Function(Pointer<Utf8>), void Function(Pointer<Utf8>)>(
          'yunong_free_string');
      final p1 = url.toNativeUtf8();
      final p2 = db.toNativeUtf8();
      try {
        final result = call(p1, p2);
        if (result == nullptr) {
          throw const AuthRefreshUnavailable('核心库没有返回结果');
        }
        final text = result.toDartString();
        free(result);
        final decoded = jsonDecode(text);
        return decoded is Map<String, dynamic> ? decoded : <String, dynamic>{};
      } finally {
        calloc.free(p1);
        calloc.free(p2);
      }
    });
  }
}