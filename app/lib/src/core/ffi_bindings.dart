// 端侧 Rust 核心的 FFI 绑定（Windows/Android 共用一套 `yunong_*` 导出）。
//
// 约定（与 `crates/core/src/ffi.rs` 对齐）：
// - 每个函数返回 **Rust 侧 malloc 的 UTF-8 JSON**，必须用 `yunong_free_string` 释放；
// - 信封：`{"ok":true,"data":…}` / `{"ok":false,"code":…,"message":…}`；
// - Rust 侧保证**绝不返回 NULL**，所以这里可以安全地读指针；
// - 返回值里没有 Cookie / API Key / HTML 原文。

import 'dart:convert';
import 'dart:ffi';

import 'package:flutter/foundation.dart';
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
typedef _ThreeArgsNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _ThreeArgsDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);

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

  /// 门户周数据 JSON 入库（W6 新形态；HTML 路径保留作兜底）。
  CoreEnvelope importScheduleJson({
    required String json,
    required String term,
    required String dbPath,
  });

  /// 设置/修正学期开始日（`YYYY-MM-DD`，空串=清空）。
  /// 导出某学期课表为 .ics（W7）；成功返回 {path, events}，不返回 NULL。
  CoreEnvelope exportIcs({
    required String term,
    required String outPath,
    required String dbPath,
  });

  CoreEnvelope setTermStart({
    required String term,
    required String startsOn,
    required String dbPath,
  });
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
            library.lookupFunction<_VersionNative, _VersionDart>('yunong_core_version'),
        _importSchedule = library
            .lookupFunction<_ThreeArgsNative, _ThreeArgsDart>('yunong_import_schedule_json'),
        _setTermStart = library
            .lookupFunction<_ThreeArgsNative, _ThreeArgsDart>('yunong_set_term_start'),
        _exportIcs = library
            .lookupFunction<_ThreeArgsNative, _ThreeArgsDart>('yunong_export_ics');

  final _ImportDart _import;
  final _TwoArgsDart _listCourses;
  final _OneArgDart _listTerms;
  final _FreeDart _free;
  final _VersionDart _version;
  final _ThreeArgsDart _importSchedule;
  final _ThreeArgsDart _setTermStart;
  final _ThreeArgsDart _exportIcs;

  static CoreLibrary? _instance;

  /// 加载（进程内只加载一次）。
  ///
  /// 查找顺序：`--dart-define=YUNONG_CORE_DLL` → 环境变量 `YUNONG_CORE_DLL`
  /// → 可执行文件同目录 → 系统库搜索路径。
  /// 打开端侧核心库（chat 流式绑定复用同一份 DLL；找不到时抛 [CoreUnavailable]）。
  static DynamicLibrary openCoreLibrary() {
    final errors = <String>[];
    for (final candidate in _candidates()) {
      try {
        return DynamicLibrary.open(candidate);
      } on ArgumentError catch (err) {
        errors.add('$candidate: ${err.message}');
      }
    }
    throw CoreUnavailable(errors.join('；'));
  }

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
      // t174：Flutter 打包偶发会再套一层 `lib` 前缀（liblibyunong_core.so），
      // 以及 Android 上部分 ROM 需要显式带 .so 后缀的绝对路径 → 一并尝试。
      if (Platform.isAndroid || Platform.isIOS) ...<String>[
        'liblibyunong_core.so',
        for (final String dir in androidNativeLibDirs()) '$dir/libyunong_core.so',
      ],
    ];
  }

  /// 【t174】Android 上可能的 native library 目录（`/data/app/<pkg>/lib/<abi>/`）。
  ///
  /// 没有插件时拿不到 `ApplicationInfo.nativeLibraryDir`，所以从两处**真实可读**的信息推断：
  /// 1. `/proc/self/maps` 里已加载库的路径（Flutter 自己的 `libflutter.so` 就在同一个目录）；
  /// 2. `Platform.resolvedExecutable` 附近的 `lib/` 目录。
  static List<String> androidNativeLibDirs() {
    if (!Platform.isAndroid) {
      return const <String>[];
    }
    final Set<String> dirs = <String>{};
    try {
      final File maps = File('/proc/self/maps');
      if (maps.existsSync()) {
        for (final String line in maps.readAsLinesSync()) {
          if (!line.contains('libflutter.so') && !line.contains('libapp.so')) {
            continue;
          }
          final int slash = line.lastIndexOf('/');
          if (slash > 0) {
            dirs.add(line.substring(0, slash));
          }
        }
      }
    } on Object catch (failure) {
      debugPrint('[core] 读 /proc/self/maps 失败：${failure.runtimeType} $failure');
    }
    try {
      final String exe = Platform.resolvedExecutable;
      final int idx = exe.indexOf('/lib/');
      if (idx > 0) {
        dirs.add(exe.substring(0, idx + 4));
      }
    } on Object {
      // resolvedExecutable 在 Android 上可能不可用 → 忽略
    }
    return dirs.toList();
  }

  /// 【t174】设备 ABI 线索（用于"本机 ABI 不在支持列表"的明确提示）。
  ///
  /// Dart 侧没有 `Build.SUPPORTED_ABIS`，但 `/proc/self/maps` 的库路径里带 ABI 目录名
  /// （`.../lib/arm64/`），`Platform.resolvedExecutable` 里也有 `app_process64` 之类的线索。
  static String? androidAbiHint() {
    if (!Platform.isAndroid) {
      return null;
    }
    try {
      final String exe = Platform.resolvedExecutable;
      if (exe.contains('app_process64')) {
        return 'arm64';
      }
      if (exe.contains('app_process32')) {
        return 'arm';
      }
    } on Object {
      // 忽略
    }
    for (final String dir in androidNativeLibDirs()) {
      for (final String abi in <String>['arm64', 'arm', 'x86_64', 'x86']) {
        if (dir.endsWith('/$abi')) {
          return abi;
        }
      }
    }
    return null;
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

  @override
  CoreEnvelope importScheduleJson({
    required String json,
    required String term,
    required String dbPath,
  }) =>
      _call(<String>[json, term, dbPath],
          (List<Pointer<Utf8>> p) => _importSchedule(p[0], p[1], p[2]));

  @override
  CoreEnvelope exportIcs({
    required String term,
    required String outPath,
    required String dbPath,
  }) =>
      _call(<String>[term, outPath, dbPath],
          (List<Pointer<Utf8>> p) => _exportIcs(p[0], p[1], p[2]));

  @override
  CoreEnvelope setTermStart({
    required String term,
    required String startsOn,
    required String dbPath,
  }) =>
      _call(<String>[term, startsOn, dbPath],
          (List<Pointer<Utf8>> p) => _setTermStart(p[0], p[1], p[2]));
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
/// 【t165】端侧核心的**可显示状态**（设置页「关于」常驻一行）。
///
/// owner 反馈"v0.9 没有『端侧核心已就绪』"：加载失败时只有一条黄色横幅，
/// 看不到失败原因（哪个路径、什么错误）。这里把 dlopen 的真实过程记下来。
class CoreStatus {
  const CoreStatus._({required this.ok, required this.detail, this.sizeBytes, this.version});

  /// 测试用构造（不触发真实 dlopen）。
  const CoreStatus.testOnly({
    required this.ok,
    required this.detail,
    this.sizeBytes,
    this.version,
  });

  /// 是否加载成功。
  final bool ok;

  /// 失败原因（含每个尝试过的路径与 `dlerror` 文本）；成功时是简短说明。
  final String detail;

  /// 库文件大小（能 stat 到时才有；Android 上可能取不到）。
  final int? sizeBytes;

  /// 核心导出的版本字符串（成功时才有）。
  final String? version;

  static CoreStatus? _latest;

  /// 最近一次探测结果（未探测过返回 null）。
  static CoreStatus? get latest => _latest;

  /// 测试/启动时注入。
  static set debugLatest(CoreStatus? value) => _latest = value;

  /// 探测一次（只做 dlopen + 版本，不建表、不读业务数据）。
  static CoreStatus probe() {
    final List<String> candidates = CoreLibrary._candidates();
    try {
      final CoreLibrary library = CoreLibrary.instance();
      String? version;
      try {
        version = library.version;
      } on Object {
        version = null;
      }
      int? size;
      for (final String path in candidates) {
        try {
          final File file = File(path);
          if (file.existsSync()) {
            size = file.lengthSync();
            break;
          }
        } on Object {
          // 路径不是文件（如纯文件名交给系统搜索）→ 跳过大小探测
        }
      }
      final CoreStatus status = CoreStatus._(
        ok: true,
        detail: version == null ? '已就绪' : '已就绪（yunong_core $version）',
        sizeBytes: size,
        version: version,
      );
      _latest = status;
      return status;
    } on CoreUnavailable catch (failure) {
      final CoreStatus status = CoreStatus._(
        ok: false,
        detail: '未加载（尝试过：${candidates.join(' / ')}；错误：${failure.detail}）',
      );
      _latest = status;
      return status;
    } on Object catch (failure) {
      final CoreStatus status = CoreStatus._(
        ok: false,
        detail: '未加载（尝试过：${candidates.join(' / ')}；错误：${failure.runtimeType}: $failure）',
      );
      _latest = status;
      return status;
    }
  }

  /// 设置页/关于页显示的一行文案。
  String get label {
    if (!ok) {
      return '端侧核心：$detail';
    }
    final int? bytes = sizeBytes;
    if (bytes == null) {
      return '端侧核心：$detail';
    }
    final String size = bytes >= 1024 * 1024
        ? '${(bytes / (1024 * 1024)).toStringAsFixed(2)} MB'
        : '${(bytes / 1024).toStringAsFixed(0)} KB';
    return '端侧核心：已就绪（yunong_core $size）';
  }
}

class CoreUnavailable implements Exception {
  const CoreUnavailable(this.detail);

  final String detail;

  @override
  String toString() => '端侧核心未就绪：$detail';
}
