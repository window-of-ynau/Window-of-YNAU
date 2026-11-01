// BYOK 凭据的 Dart 侧（W7 P0）：只调三个 FFI，**明文永远不经过 Dart**。
//
// 设计要点：
// - `status()` 只拿到 `{set, masked, degraded, reason}` —— 没有明文可取，UI 也就不可能回显；
// - 写入时把明文直接交给 Rust（一次 FFI 调用），Dart 不保留副本；
// - 平台不可用时 Rust 返回 `degraded:true` + `reason`，UI 明确显示“本次仅内存生效”。

import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import 'ffi_bindings.dart';

/// 凭据状态（**不含明文**）。
class SecretStatus {
  const SecretStatus({
    required this.set,
    required this.masked,
    this.degraded = false,
    this.reason,
  });

  factory SecretStatus.fromJson(Map<String, dynamic> json) => SecretStatus(
        set: json['set'] == true,
        masked: (json['masked'] as String?) ?? '',
        degraded: json['degraded'] == true,
        reason: json['reason'] as String?,
      );

  final bool set;

  /// 形如 `sk-****3eee`
  final String masked;

  /// true = 只存在内存里（重启失效）
  final bool degraded;
  final String? reason;

  String get display => set ? masked : '未设置';

  /// 降级时的提示文案（Rust 给 reason 就用它，否则用通用文案）
  String get degradedHint => reason ?? '本次仅内存生效，重启需重新填写';
}

/// 凭据后端接口（测试用 fake，不需要真 DLL）。
abstract class SecretBackend {
  /// 写入；返回状态（含掩码/降级）。
  Map<String, dynamic> setSecret({required String key, required String value, required String dbPath});

  Map<String, dynamic> getSecret({required String key, required String dbPath});

  Map<String, dynamic> deleteSecret({required String key, required String dbPath});
}

/// 真 FFI 后端：`yunong_set_secret` / `yunong_get_secret` / `yunong_delete_secret`。
class FfiSecretBackend implements SecretBackend {
  FfiSecretBackend() {
    final library = CoreLibrary.openCoreLibrary();
    _set = library.lookupFunction<_ThreeArgNative, _ThreeArgDart>('yunong_set_secret');
    _get = library.lookupFunction<_TwoArgNative, _TwoArgDart>('yunong_get_secret');
    _delete = library.lookupFunction<_TwoArgNative, _TwoArgDart>('yunong_delete_secret');
    _free = library.lookupFunction<_FreeNative, _FreeDart>('yunong_free_string');
  }

  late final _ThreeArgDart _set;
  late final _TwoArgDart _get;
  late final _TwoArgDart _delete;
  late final _FreeDart _free;

  Map<String, dynamic> _call(
    Pointer<Utf8> Function(Pointer<Utf8> a, Pointer<Utf8> b) invoke,
    String first,
    String second,
  ) {
    final p1 = first.toNativeUtf8();
    final p2 = second.toNativeUtf8();
    try {
      final result = invoke(p1, p2);
      if (result == nullptr) {
        return <String, dynamic>{'ok': false, 'code': 'INTERNAL', 'message': '核心库没有返回结果'};
      }
      final text = result.toDartString();
      _free(result);
      return jsonDecode(text) as Map<String, dynamic>;
    } finally {
      calloc.free(p1);
      calloc.free(p2);
    }
  }

  Map<String, dynamic> _call3(
    Pointer<Utf8> Function(Pointer<Utf8> a, Pointer<Utf8> b, Pointer<Utf8> c) invoke,
    String first,
    String second,
    String third,
  ) {
    final p1 = first.toNativeUtf8();
    final p2 = second.toNativeUtf8();
    final p3 = third.toNativeUtf8();
    try {
      final result = invoke(p1, p2, p3);
      if (result == nullptr) {
        return <String, dynamic>{'ok': false, 'code': 'INTERNAL', 'message': '核心库没有返回结果'};
      }
      final text = result.toDartString();
      _free(result);
      return jsonDecode(text) as Map<String, dynamic>;
    } finally {
      calloc.free(p1);
      calloc.free(p2);
      calloc.free(p3);
    }
  }

  @override
  Map<String, dynamic> setSecret({
    required String key,
    required String value,
    required String dbPath,
  }) =>
      _call3((Pointer<Utf8> a, Pointer<Utf8> b, Pointer<Utf8> c) => _set(a, b, c), key, value, dbPath);

  @override
  Map<String, dynamic> getSecret({required String key, required String dbPath}) =>
      _call((Pointer<Utf8> a, Pointer<Utf8> b) => _get(a, b), key, dbPath);

  @override
  Map<String, dynamic> deleteSecret({required String key, required String dbPath}) =>
      _call((Pointer<Utf8> a, Pointer<Utf8> b) => _delete(a, b), key, dbPath);
}

typedef _ThreeArgNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _ThreeArgDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _TwoArgNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>);
typedef _TwoArgDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<Utf8>);
typedef _FreeDart = void Function(Pointer<Utf8>);

/// 凭据服务：UI 只依赖它。
class SecretService {
  SecretService({SecretBackend? backend, String? dbPath})
      : _backend = backend ?? FfiSecretBackend(),
        _dbPath = dbPath ?? '';

  final SecretBackend _backend;
  final String _dbPath;

  /// BYOK Key 在安全存储里的固定 key 名。
  static const String byokKeyName = 'byok_api_key';

  SecretStatus _parse(Map<String, dynamic> json) {
    if (json['ok'] == false) {
      throw SecretException(
        code: (json['code'] as String?) ?? 'INTERNAL',
        message: (json['message'] as String?) ?? '',
      );
    }
    return SecretStatus.fromJson((json['data'] as Map<String, dynamic>?) ?? <String, dynamic>{});
  }

  Future<SecretStatus> status(String key) async =>
      _parse(_backend.getSecret(key: key, dbPath: _dbPath));

  /// 写入凭据。**调用方不应把明文存到任何 Dart 长期状态里**。
  Future<SecretStatus> save(String key, String value) async =>
      _parse(_backend.setSecret(key: key, value: value, dbPath: _dbPath));

  Future<bool> delete(String key) async {
    final json = _backend.deleteSecret(key: key, dbPath: _dbPath);
    if (json['ok'] == false) {
      throw SecretException(
        code: (json['code'] as String?) ?? 'INTERNAL',
        message: (json['message'] as String?) ?? '',
      );
    }
    return ((json['data'] as Map<String, dynamic>?) ?? <String, dynamic>{})['deleted'] == true;
  }
}

/// 凭据操作失败（文案按错误码给，不显示原始错误串）。
class SecretException implements Exception {
  const SecretException({required this.code, required this.message});

  final String code;
  final String message;

  String get userHint => switch (code) {
        'BAD_INPUT' => 'Key 名不合法或内容为空/过长',
        'STORAGE' => '凭据存储失败，请重试',
        _ => '保存失败，请稍后重试',
      };

  @override
  String toString() => 'SecretException($code)';
}
