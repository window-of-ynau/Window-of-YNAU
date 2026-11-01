// 端侧 Rust 核心的 FFI 绑定（Windows/Android 共用一套 `yunong_*` 导出）。
//
// 约定（与 `crates/core/src/ffi.rs` 对齐）：
// - 每个函数返回 **Rust 侧 malloc 的 UTF-8 JSON**，必须用 `yunong_free_string` 释放；
// - 信封：`{"ok":true,"data":…}` / `{"ok":false,"code":…,"message":…}`；
// - Rust 侧保证**绝不返回 NULL**，所以这里可以安全地读指针；
// - 返回值里没有 Cookie / API Key / HTML 原文。

import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

/// 一条 JSON 信封。
class CoreEnvelope {
  const CoreEnvelope({required this.ok, this.data, this.code, this.message});

  final bool ok;
  final Map<String, dynamic>? data;
  final String? code;
  final String? message;

  factory CoreEnvelope.parse(String text) {
    final decoded = jsonDecode(text);
    if (decoded is! Map<String, dynamic>) {
      throw const FormatException('核心返回的不是 JSON 对象');
    }
    final data = decoded['data'];
    return CoreEnvelope(
      ok: decoded['ok'] == true,
      data: data is Map<String, dynamic> ? data : null,
      code: decoded['code'] as String?,
      message: decoded['message'] as String?,
    );
  }
}

typedef _ImportNative = Pointer<Utf8> Function(
    Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _ImportDart = Pointer<Utf8> Function(
    Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _TwoArgsNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>);
typedef _TwoArgsDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>);
typedef _OneArgNative = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _OneArgDart = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<Utf8>);
typedef _FreeDart = void Function(Pointer<Utf8>);
typedef _VersionNative = Pointer<Utf8> Function();
typedef _VersionDart = Pointer<Utf8> Function();

/// 核心能力接口：生产实现是 [CoreLibrary]，测试用 fake（不需要真 DLL）。
abstract class CoreApi {
  String get version;

  CoreEnvelope importTimetable({
    required String html,
    required String term,
    required String dbPath,
  });

  CoreEnvelope listCourses({required String term, required String dbPath});

  CoreEnvelope listTerms({required String dbPath});
}

/// 已加载的核心库。
class CoreLibrary implements CoreApi {
  CoreLibrary._(DynamicLibrary library)
      : _import = library.lookupFunction<_ImportNative, _ImportDart>(
            'yunong_import_timetable'),
        _listCourses = library.lookupFunction<_TwoArgsNative, _TwoArgsDart>(
            'yunong_list_courses'),
        _listTerms =
            library.lookupFunction<_OneArgNative, _OneArgDart>('yunong_list_terms'),
        _free = library.lookupFunction<_FreeNative, _FreeDart>(
            'yunong_free_string'),
        _version =
            library.lookupFunction<_VersionNative, _VersionDart>('yunong_core_version');

  final _ImportDart _import;
  final _TwoArgsDart _listCourses;
  final _OneArgDart _listTerms;
  final _FreeDart _free;
  final _VersionDart _version;

  static CoreLibrary? _instance;

  /// 加载（进程内只加载一次）。
  ///
  /// 查找顺序：`--dart-define=YUNONG_CORE_DLL` → 环境变量 `YUNONG_CORE_DLL`
  /// → 可执行文件同目录 → 系统库搜索路径。
  static CoreLibrary instance() {
    final cached = _instance;
    if (cached != null) {
      return cached;
    }
    final errors = <String>[];
    for (final candidate in _candidates()) {
      try {
        final library = CoreLibrary._(DynamicLibrary.open(candidate));
        _instance = library;
        return library;
      } on ArgumentError catch (err) {
        errors.add('$candidate: ${err.message}');
      }
    }
    throw CoreUnavailable(errors.join('；'));
  }

  static List<String> _candidates() {
    const fromDefine = String.fromEnvironment('YUNONG_CORE_DLL');
    final fromEnv = Platform.environment['YUNONG_CORE_DLL'] ?? '';
    final fileName = Platform.isWindows
        ? 'yunong_core.dll'
        : Platform.isMacOS
            ? 'libyunong_core.dylib'
            : 'libyunong_core.so';
    final exeDir = File(Platform.resolvedExecutable).parent.path;
    return <String>[
      if (fromDefine.isNotEmpty) fromDefine,
      if (fromEnv.isNotEmpty) fromEnv,
      if (!Platform.isAndroid && !Platform.isIOS)
        '$exeDir${Platform.pathSeparator}$fileName',
      fileName,
    ];
  }

  @override
  String get version => _version().toDartString();

  @override
  CoreEnvelope importTimetable({
    required String html,
    required String term,
    required String dbPath,
  }) =>
      _call(
        <String>[html, term, dbPath],
        (List<Pointer<Utf8>> p) => _import(p[0], p[1], p[2]),
      );

  @override
  CoreEnvelope listCourses({required String term, required String dbPath}) =>
      _call(<String>[term, dbPath],
          (List<Pointer<Utf8>> p) => _listCourses(p[0], p[1]));

  @override
  CoreEnvelope listTerms({required String dbPath}) =>
      _call(<String>[dbPath], (List<Pointer<Utf8>> p) => _listTerms(p[0]));

  CoreEnvelope _call(
    List<String> args,
    Pointer<Utf8> Function(List<Pointer<Utf8>>) invoke,
  ) {
    final allocated = <Pointer<Utf8>>[];
    try {
      for (final arg in args) {
        allocated.add(arg.toNativeUtf8());
      }
      final out = invoke(allocated);
      if (out.address == 0) {
        // Rust 侧承诺不返回 NULL；真出现说明 DLL 不是我们那份，别静默。
        throw const FormatException('核心返回了 NULL（DLL 版本不匹配？）');
      }
      try {
        return CoreEnvelope.parse(out.toDartString());
      } finally {
        _free(out);
      }
    } finally {
      for (final pointer in allocated) {
        calloc.free(pointer);
      }
    }
  }
}

/// 核心 DLL 加载失败（打包缺文件 / 架构不匹配）。
class CoreUnavailable implements Exception {
  const CoreUnavailable(this.detail);

  final String detail;

  @override
  String toString() => '端侧核心未就绪：$detail';
}
