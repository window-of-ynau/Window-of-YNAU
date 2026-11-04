// 供应商（provider）模型与存储（t115）。
//
// 为什么单独一层：
// - 设置页只该出现**一处**能填 Key 的地方（owner 批评过“两个可以填写 BYOK 的位置”）；
// - Key 仍然只存 DPAPI（`SecretService`），这里只存**非敏感的元数据**（名称/base_url/模型列表）；
// - 迁移：老版本只有一个 `byok_api_key`，它现在就是「默认供应商」的 Key —— 用户不需要重填。

import 'dart:convert';

import 'package:flutter/foundation.dart';

import '../theme/theme_controller.dart';
import 'secret_service.dart';

/// 一个供应商。
class ProviderConfig {
  const ProviderConfig({
    required this.id,
    required this.name,
    required this.baseUrl,
    this.models = const <String>[],
  });

  factory ProviderConfig.fromJson(Map<String, dynamic> json) => ProviderConfig(
        id: (json['id'] as String?) ?? '',
        name: (json['name'] as String?) ?? '',
        baseUrl: (json['base_url'] as String?) ?? '',
        models: ((json['models'] as List<dynamic>?) ?? <dynamic>[])
            .map((dynamic item) => '$item')
            .where((String item) => item.isNotEmpty)
            .toList(growable: false),
      );

  /// 稳定 id（也是 Key 的存储名后缀）。
  final String id;
  final String name;
  final String baseUrl;

  /// 自定义模型列表（空 = 用内置默认）。
  final List<String> models;

  ProviderConfig copyWith({String? name, String? baseUrl, List<String>? models}) => ProviderConfig(
        id: id,
        name: name ?? this.name,
        baseUrl: baseUrl ?? this.baseUrl,
        models: models ?? this.models,
      );

  Map<String, dynamic> toJson() => <String, dynamic>{
        'id': id,
        'name': name,
        'base_url': baseUrl,
        'models': models,
      };

  /// Key 在 DPAPI 里的名字。
  ///
  /// **默认供应商**沿用老的 `byok_api_key`：这样升级后用户已保存的 Key 直接可用，
  /// 不需要重新填写（迁移要求）。
  String get secretKeyName =>
      id == ProviderStore.defaultProviderId ? SecretService.byokKeyName : 'provider_$id';
}

/// 供应商列表（可增删改）+ 默认供应商迁移。
class ProviderStore extends ChangeNotifier {
  ProviderStore({SettingsStore? store, SecretService? secrets})
      : _store = store,
        _secrets = secrets;

  static const String storageKey = 'chat.providers';
  static const String defaultProviderId = 'default';

  final SettingsStore? _store;
  final SecretService? _secrets;

  List<ProviderConfig> _providers = const <ProviderConfig>[
    ProviderConfig(id: defaultProviderId, name: '默认供应商', baseUrl: 'https://api.deepseek.com'),
  ];
  String _activeId = defaultProviderId;

  List<ProviderConfig> get providers => List<ProviderConfig>.unmodifiable(_providers);
  String get activeId => _activeId;
  ProviderConfig get active => _providers.firstWhere(
        (ProviderConfig provider) => provider.id == _activeId,
        orElse: () => _providers.first,
      );

  /// 从设置里读回列表；没有则保持「默认供应商」。
  Future<void> load() async {
    final String? raw = await _store?.get(storageKey);
    if (raw == null || raw.isEmpty) {
      return;
    }
    try {
      final dynamic decoded = jsonDecode(raw);
      if (decoded is! Map<String, dynamic>) {
        return;
      }
      final List<dynamic> list = (decoded['list'] as List<dynamic>?) ?? <dynamic>[];
      final List<ProviderConfig> parsed = list
          .whereType<Map<String, dynamic>>()
          .map(ProviderConfig.fromJson)
          .where((ProviderConfig provider) => provider.id.isNotEmpty)
          .toList();
      if (parsed.isEmpty) {
        return;
      }
      _providers = parsed;
      final String? active = decoded['active'] as String?;
      if (active != null && parsed.any((ProviderConfig p) => p.id == active)) {
        _activeId = active;
      }
      notifyListeners();
    } on FormatException {
      // 坏数据不阻塞启动：保持默认。
    }
  }

  Future<void> _persist() async {
    final String raw = jsonEncode(<String, dynamic>{
      'active': _activeId,
      'list': _providers.map((ProviderConfig provider) => provider.toJson()).toList(),
    });
    await _store?.set(storageKey, raw);
  }

  Future<void> add(ProviderConfig provider) async {
    _providers = <ProviderConfig>[..._providers, provider];
    _activeId = provider.id;
    await _persist();
    notifyListeners();
  }

  Future<void> update(ProviderConfig provider) async {
    _providers = _providers
        .map((ProviderConfig item) => item.id == provider.id ? provider : item)
        .toList();
    await _persist();
    notifyListeners();
  }

  Future<void> remove(String id) async {
    if (_providers.length <= 1) {
      return; // 至少留一个，否则没有地方填 Key
    }
    _providers = _providers.where((ProviderConfig provider) => provider.id != id).toList();
    if (_activeId == id) {
      _activeId = _providers.first.id;
    }
    await _persist();
    notifyListeners();
  }

  Future<void> setActive(String id) async {
    if (!_providers.any((ProviderConfig provider) => provider.id == id)) {
      return;
    }
    _activeId = id;
    await _persist();
    notifyListeners();
  }

  /// 迁移：把老 Key 视为「默认供应商」的 Key（只读判断，不改用户已存的值）。
  ///
  /// 返回 true 表示老 Key 仍然可用 → UI 直接显示「已保存（沿用旧 Key）」。
  Future<bool> legacyKeyAvailable() async {
    final SecretService? secrets = _secrets;
    if (secrets == null) {
      return false;
    }
    try {
      final SecretStatus status = await secrets.status(SecretService.byokKeyName);
      return status.set && !status.broken;
    } on Object {
      return false;
    }
  }
}

/// 协议自动协商的展示文案（只读）。
///
/// 真正的解析在 Rust 侧（responses → messages → completions）；客户端**不提供选择**。
String protocolAutoLabel(String providerName) => '当前生效：自动（$providerName）';
