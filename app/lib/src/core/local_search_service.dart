// 本地搜索入口（W7）：**本地资料**检索，与联网搜索是两回事（界面必须分清）。
//
// 引擎：`yunong_search_local(query, limit, db_path)`（本地 BM25，core-rust 的 t43）。
// 该符号尚未合入时，`FfiLocalSearchBackend` 会抛 `LocalSearchUnavailable`，
// UI 诚实显示"本地搜索还没就绪"，**不自己造一套检索**、也不假装搜到。
//
// 数据局部性：本地搜索只读端侧 SQLite，结果**不来自网络**（ADR-19 原则 1）。

import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import 'ffi_bindings.dart';

/// 一条本地检索结果。
class LocalSearchHit {
  const LocalSearchHit({
    required this.title,
    required this.snippet,
    this.score = 0,
    this.id = 0,
  });

  factory LocalSearchHit.fromJson(Map<String, dynamic> json) => LocalSearchHit(
        title: (json['title'] as String?) ?? '',
        snippet: (json['snippet'] as String?) ?? (json['content'] as String?) ?? '',
        score: (json['score'] as num?)?.toDouble() ?? 0,
        id: (json['id'] as num?)?.toInt() ?? 0,
      );

  final String title;
  final String snippet;
  final double score;
  final int id;

  String get displayTitle => title.trim().isEmpty ? '（无标题）' : title;

  /// 插入输入框的引用文本（带上来源标题，便于模型区分"这是资料"）。
  String get citation => '【本地资料·$displayTitle】$snippet';
}

/// 本地搜索后端（可替换：测试用 fake）。
abstract class LocalSearchBackend {
  /// 返回命中列表；`query` 为空时返回空列表。
  List<Map<String, dynamic>> search({
    required String query,
    required int limit,
    required String dbPath,
  });
}

/// 本地搜索还没就绪（符号未合入 / 核心库不可用）。
class LocalSearchUnavailable implements Exception {
  const LocalSearchUnavailable(this.reason);

  final String reason;

  String get userHint => '本地搜索还没就绪：$reason';

  @override
  String toString() => 'LocalSearchUnavailable($reason)';
}

/// 真 FFI：`yunong_search_local(query, limit, db_path)`。
class FfiLocalSearchBackend implements LocalSearchBackend {
  FfiLocalSearchBackend() {
    final library = CoreLibrary.openCoreLibrary();
    try {
      _search = library.lookupFunction<_ThreeArgNative, _ThreeArgDart>('yunong_search_local');
    } on ArgumentError {
      // 符号还没合入：明确报"未就绪"，不偷偷返回空结果（那会让人以为"没命中"）。
      throw const LocalSearchUnavailable('端侧核心还没有 yunong_search_local');
    }
    _free = library.lookupFunction<_FreeNative, _FreeDart>('yunong_free_string');
  }

  late final _ThreeArgDart _search;
  late final _FreeDart _free;

  @override
  List<Map<String, dynamic>> search({
    required String query,
    required int limit,
    required String dbPath,
  }) {
    final p1 = query.toNativeUtf8();
    final p2 = '$limit'.toNativeUtf8();
    final p3 = dbPath.toNativeUtf8();
    try {
      final result = _search(p1, p2, p3);
      if (result == nullptr) {
        throw const LocalSearchUnavailable('核心库没有返回结果');
      }
      final text = result.toDartString();
      _free(result);
      final json = jsonDecode(text) as Map<String, dynamic>;
      if (json['ok'] == false) {
        throw LocalSearchUnavailable((json['message'] as String?) ?? '搜索失败');
      }
      final data = json['data'];
      final hits = data is Map<String, dynamic> ? data['hits'] : data;
      if (hits is! List) {
        return <Map<String, dynamic>>[];
      }
      return hits.whereType<Map<String, dynamic>>().toList();
    } finally {
      calloc.free(p1);
      calloc.free(p2);
      calloc.free(p3);
    }
  }
}

typedef _ThreeArgNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _ThreeArgDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<Utf8>);
typedef _FreeDart = void Function(Pointer<Utf8>);

/// 本地搜索服务：UI 只依赖它。
class LocalSearchService {
  LocalSearchService({LocalSearchBackend? backend, String? dbPath})
      : _backend = backend,
        _dbPath = dbPath ?? '';

  /// null = 还没创建（懒加载 FFI 后端，避免构造时抛错影响界面）。
  final LocalSearchBackend? _backend;
  final String _dbPath;

  LocalSearchBackend _resolve() {
    final injected = _backend;
    if (injected != null) {
      return injected;
    }
    return FfiLocalSearchBackend();
  }

  /// 检索；未就绪时抛 `LocalSearchUnavailable`（UI 显示"还没就绪"，不谎称无命中）。
  Future<List<LocalSearchHit>> search(String query, {int limit = 8}) async {
    final trimmed = query.trim();
    if (trimmed.isEmpty) {
      return const <LocalSearchHit>[];
    }
    final hits = _resolve().search(query: trimmed, limit: limit, dbPath: _dbPath);
    return hits.map(LocalSearchHit.fromJson).toList();
  }
}
