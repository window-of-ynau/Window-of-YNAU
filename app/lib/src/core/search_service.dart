// 联网搜索的 Dart 侧（ADR-19 决策 5）：**诚实**是第一位的。
//
// 四通道优先级由 Rust 的 `yunong_search_status` 统一判定（单一事实来源），
// Dart 只负责展示：`内置` / `我方代理` / `自带 Key` / **`未配置`**。
// 未配置时 UI 必须常驻提示"未配置联网搜索，模型无法获取实时信息"——禁止静默降级。

import 'dart:convert';
import 'dart:ffi';

import 'package:ffi/ffi.dart';

import 'ffi_bindings.dart';

/// 搜索来源（与 Rust 的 `SearchSource` 一一对应）。
enum SearchSource {
  builtIn('built_in', '内置'),
  proxy('proxy', '我方代理'),
  byok('byok', '自带 Key'),
  none('none', '未配置');

  const SearchSource(this.wire, this.label);

  final String wire;
  final String label;

  bool get available => this != SearchSource.none;

  static SearchSource fromWire(String? wire) => SearchSource.values.firstWhere(
        (SearchSource value) => value.wire == wire,
        orElse: () => SearchSource.none,
      );
}

/// 搜索状态（UI 直接渲染；`available:false` 时**必须**显示 `hint`）。
class SearchStatus {
  const SearchStatus({
    required this.source,
    required this.label,
    required this.available,
    this.hint = '',
  });

  /// 诚实兜底：核心库不可用时一律显示"未配置"，绝不假装可用。
  const SearchStatus.unknown()
      : source = SearchSource.none,
        label = '未配置',
        available = false,
        hint = '未配置联网搜索，模型无法获取实时信息';

  factory SearchStatus.fromJson(Map<String, dynamic> data) {
    final source = SearchSource.fromWire(data['source'] as String?);
    return SearchStatus(
      source: source,
      label: (data['label'] as String?) ?? source.label,
      available: data['available'] == true,
      hint: (data['hint'] as String?) ?? '',
    );
  }

  final SearchSource source;
  final String label;
  final bool available;
  final String hint;

  String get display => label;
}

/// 搜索状态后端（测试用 fake）。
abstract class SearchBackend {
  Map<String, dynamic> status({
    required String channel,
    required String protocol,
    required String dbPath,
  });
}

/// 真 FFI 后端：`yunong_search_status(channel, protocol, db_path)`。
class FfiSearchBackend implements SearchBackend {
  FfiSearchBackend() {
    final library = CoreLibrary.openCoreLibrary();
    _status = library.lookupFunction<_StatusNative, _StatusDart>('yunong_search_status');
    _free = library.lookupFunction<_FreeNative, _FreeDart>('yunong_free_string');
  }

  late final _StatusDart _status;
  late final _FreeDart _free;

  @override
  Map<String, dynamic> status({
    required String channel,
    required String protocol,
    required String dbPath,
  }) {
    final p1 = channel.toNativeUtf8();
    final p2 = protocol.toNativeUtf8();
    final p3 = dbPath.toNativeUtf8();
    try {
      final result = _status(p1, p2, p3);
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
}

typedef _StatusNative = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _StatusDart = Pointer<Utf8> Function(Pointer<Utf8>, Pointer<Utf8>, Pointer<Utf8>);
typedef _FreeNative = Void Function(Pointer<Utf8>);
typedef _FreeDart = void Function(Pointer<Utf8>);

/// 搜索状态服务：UI 只依赖它。
class SearchService {
  SearchService({SearchBackend? backend, String? dbPath})
      : _backend = backend ?? FfiSearchBackend(),
        _dbPath = dbPath ?? '';

  final SearchBackend _backend;
  final String _dbPath;

  /// 查询当前通道/协议下的搜索来源。核心库不可用时返回 `SearchStatus.unknown()`
  /// （显示"未配置"，而不是假装"内置/代理"可用）。
  Future<SearchStatus> status({required String channel, required String protocol}) async {
    try {
      final json = _backend.status(channel: channel, protocol: protocol, dbPath: _dbPath);
      if (json['ok'] == false) {
        return const SearchStatus.unknown();
      }
      final data = json['data'];
      if (data is! Map<String, dynamic>) {
        return const SearchStatus.unknown();
      }
      return SearchStatus.fromJson(data);
    } on Object {
      return const SearchStatus.unknown();
    }
  }
}
