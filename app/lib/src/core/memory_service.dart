// 本地记忆（W7）：列表 / 新增 / 编辑 / 删除 + 到期过滤。
//
// **本地能力，不受档位影响**（ADR-19 原则 1）：只读写端侧 SQLite，不上云。

import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import 'ffi_bindings.dart';

/// 记忆种类（与 Rust `MemoryKind` 一致：note/summary/fact）。
enum MemoryKind {
  note('note', '笔记'),
  summary('summary', '摘要'),
  fact('fact', '事实');

  const MemoryKind(this.wire, this.label);

  final String wire;
  final String label;

  static MemoryKind parse(String? wire) => MemoryKind.values.firstWhere(
        (MemoryKind value) => value.wire == wire,
        orElse: () => MemoryKind.note,
      );
}

/// 一条本地记忆。
class MemoryEntry {
  const MemoryEntry({
    required this.id,
    required this.kind,
    required this.title,
    required this.content,
    this.source = '',
    this.updatedAt = 0,
    this.expiresAt,
  });

  factory MemoryEntry.fromJson(Map<String, dynamic> json) => MemoryEntry(
        id: (json['id'] as num?)?.toInt() ?? 0,
        kind: MemoryKind.parse(json['kind'] as String?),
        title: (json['title'] as String?) ?? '',
        content: (json['content'] as String?) ?? '',
        source: (json['source'] as String?) ?? '',
        updatedAt: (json['updated_at'] as num?)?.toInt() ?? 0,
        expiresAt: (json['expires_at'] as num?)?.toInt(),
      );

  final int id;
  final MemoryKind kind;
  final String title;
  final String content;
  final String source;

  /// Unix 秒
  final int updatedAt;
  final int? expiresAt;

  bool get expired {
    final expiry = expiresAt;
    if (expiry == null || expiry <= 0) {
      return false;
    }
    return expiry <= DateTime.now().millisecondsSinceEpoch ~/ 1000;
  }

  /// 列表预览：单行、截断（不显示全文，避免一屏只看得到一条）。
  String get preview {
    final text = content.replaceAll(RegExp(r'\s+'), ' ').trim();
    return text.length <= 80 ? text : '${text.substring(0, 80)}…';
  }

  String get displayTitle => title.trim().isEmpty ? '（无标题）' : title;
}

/// 记忆 API（可替换：测试用 fake，不需要真 DLL）。
abstract class MemoryApi {
  List<Map<String, dynamic>> list({String kind = '', int limit = 200, required String dbPath});

  /// `idOrEmpty` 为空 = 新增；非空 = 更新（不存在时后端报 BAD_INPUT，不静默新增）。
  Map<String, dynamic> upsert({
    required String kind,
    required String title,
    required String content,
    required String source,
    required String idOrEmpty,
    required String dbPath,
  });

  Map<String, dynamic> delete({required String id, required String dbPath});
}

/// 真 FFI：`yunong_memory_list` / `yunong_memory_upsert` / `yunong_memory_delete`。
class FfiMemoryApi implements MemoryApi {
  FfiMemoryApi() {
    final library = CoreLibrary.openCoreLibrary();
    _list = library.lookupFunction<_ThreeArgNative, _ThreeArgDart>('yunong_memory_list');
    _upsert = library.lookupFunction<_SixArgNative, _SixArgDart>('yunong_memory_upsert');
    _delete = library.lookupFunction<_TwoArgNative, _TwoArgDart>('yunong_memory_delete');
    _free = library.lookupFunction<_FreeNative, _FreeDart>('yunong_free_string');
  }

  late final _ThreeArgDart _list;
  late final _SixArgDart _upsert;
  late final _TwoArgDart _delete;
  late final _FreeDart _free;

  Map<String, dynamic> _decode(Pointer<Utf8> result) {
    if (result == nullptr) {
      return <String, dynamic>{'ok': false, 'code': 'INTERNAL', 'message': '核心库没有返回结果'};
    }
    final text = result.toDartString();
    _free(result);
    return jsonDecode(text) as Map<String, dynamic>;
  }

  @override
  List<Map<String, dynamic>> list({
    String kind = '',
    int limit = 200,
    required String dbPath,
  }) {
    final p1 = kind.toNativeUtf8();
    final p2 = '$limit'.toNativeUtf8();
    final p3 = dbPath.toNativeUtf8();
    try {
      final json = _decode(_list(p1, p2, p3));
      if (json['ok'] == false) {
        throw MemoryException(
          code: (json['code'] as String?) ?? 'INTERNAL',
          message: (json['message'] as String?) ?? '',
        );
      }
      final data = json['data'];
      if (data is! List) {
        return <Map<String, dynamic>>[];
      }
      return data.whereType<Map<String, dynamic>>().toList();
    } finally {
      calloc.free(p1);
      calloc.free(p2);
      calloc.free(p3);
    }
  }

  @override
  Map<String, dynamic> upsert({
    required String kind,
    required String title,
    required String content,
    required String source,
    required String idOrEmpty,
    required String dbPath,
  }) {
    final p1 = kind.toNativeUtf8();
    final p2 = title.toNativeUtf8();
    final p3 = content.toNativeUtf8();
    final p4 = source.toNativeUtf8();
    final p5 = idOrEmpty.toNativeUtf8();
    final p6 = dbPath.toNativeUtf8();
    try {
      final json = _decode(_upsert(p1, p2, p3, p4, p5, p6));
      if (json['ok'] == false) {
        throw MemoryException(
          code: (json['code'] as String?) ?? 'INTERNAL',
          message: (json['message'] as String?) ?? '',
        );
      }
      return (json['data'] as Map<String, dynamic>?) ?? <String, dynamic>{};
    } finally {
      calloc.free(p1);
      calloc.free(p2);
      calloc.free(p3);
      calloc.free(p4);
      calloc.free(p5);
      calloc.free(p6);
    }
  }

  @override
  Map<String, dynamic> delete({required String id, required String dbPath}) {
    final p1 = id.toNativeUtf8();
    final p2 = dbPath.toNativeUtf8();
    try {
      final json = _decode(_delete(p1, p2));
      if (json['ok'] == false) {
        throw MemoryException(
          code: (json['code'] as String?) ?? 'INTERNAL',
          message: (json['message'] as String?) ?? '',
        );
      }
      return (json['data'] as Map<String, dynamic>?) ?? <String, dynamic>{};
    } finally {
      calloc.free(p1);
      calloc.free(p2);
    }
  }
}

typedef _ThreeArgNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _ThreeArgDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _TwoArgNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>);
typedef _TwoArgDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>);
typedef _SixArgNative = Pointer<Utf8> Function(
    Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _SixArgDart = Pointer<Utf8> Function(
    Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<Utf8>);
typedef _FreeDart = void Function(Pointer<Utf8>);

/// 记忆操作失败（文案按错误码给，不显示原始错误串）。
class MemoryException implements Exception {
  const MemoryException({required this.code, required this.message});

  final String code;
  final String message;

  String get userHint => switch (code) {
        'BAD_INPUT' => '内容不合法（种类/长度/id 检查失败）',
        'STORAGE' => '本地数据库读写失败，请重试',
        _ => '操作失败，请稍后重试',
      };

  @override
  String toString() => 'MemoryException($code)';
}

/// 记忆服务：UI 只依赖它。
class MemoryService {
  MemoryService({MemoryApi? api, String? dbPath})
      : _api = api ?? FfiMemoryApi(),
        _dbPath = dbPath ?? '';

  final MemoryApi _api;
  final String _dbPath;

  /// 列表（`updated_at` 降序；`kind` 空串 = 全部种类）。
  Future<List<MemoryEntry>> list({MemoryKind? kind, bool hideExpired = true}) async {
    final raw = _api.list(kind: kind?.wire ?? '', limit: 200, dbPath: _dbPath);
    final entries = raw.map(MemoryEntry.fromJson).toList()
      ..sort((MemoryEntry a, MemoryEntry b) => b.updatedAt.compareTo(a.updatedAt));
    return hideExpired ? entries.where((MemoryEntry entry) => !entry.expired).toList() : entries;
  }

  /// 新增（`entry == null`）或编辑（传 `entry.id`）。
  Future<int> save({
    MemoryEntry? entry,
    required MemoryKind kind,
    required String title,
    required String content,
  }) async {
    final data = _api.upsert(
      kind: kind.wire,
      title: title,
      content: content,
      source: 'user',
      idOrEmpty: entry == null ? '' : '${entry.id}',
      dbPath: _dbPath,
    );
    return (data['id'] as num?)?.toInt() ?? 0;
  }

  Future<bool> delete(MemoryEntry entry) async {
    final data = _api.delete(id: '${entry.id}', dbPath: _dbPath);
    return data['deleted'] == true || data['ok'] == true;
  }
}
