// 供应商（provider）模型与存储（t115 建立；t125 按官方文档修正模型名）。
//
// # 内置预设的**唯一事实来源**（不要在别处再写一遍模型名）
//
// 来源：DeepSeek 官方 API 文档（模型 & 价格页）
//   - https://api-docs.deepseek.com/
//   - https://api-docs.deepseek.com/quick_start/pricing
// 查阅日期：2026-11（文档 2026-11 版）
//
// 文档明确：
//   - 模型只有两个：`deepseek-flash`（默认推荐）与 `deepseek-v4-pro`；
//   - BASE URL（OpenAI 兼容）：https://api.deepseek.com
//   - BASE URL（Anthropic 兼容）：https://api.deepseek.com/anthropic
//   - 上下文 1M / 最大输出 384K；
//   - 两个模型都支持 Tool Calls、Responses API、Anthropic API；
//   - 思考模式默认开启；
//   - 图像理解：**仅 Flash 支持**，Pro 不支持；
//   - 旧名 `deepseek-v4-flash`、`deepseek-v4-flash-vision-exp` 仍可调用（映射到 Flash）；
//   - **不存在** `deepseek-chat` / `deepseek-reasoner`（本项目早期凭空写的，已移除）。
//
// Key 仍然只存 DPAPI（`SecretService`），这里只存**非敏感元数据**（名称/base_url/模型列表）。
// 迁移：老版本只有一个 `byok_api_key`，它现在就是「默认供应商」的 Key —— 用户不需要重填。

import 'dart:convert';

import 'package:flutter/foundation.dart';

import '../theme/theme_controller.dart';
import 'secret_service.dart';

/// 内置 DeepSeek 预设（值来自上面注释里的官方文档，勿改）。
///
/// | 字段 | 值 | 文档出处 |
/// |---|---|---|
/// | name | DeepSeek | 文档页标题 |
/// | baseUrl | https://api.deepseek.com | 「BASE URL（OpenAI 格式）」 |
/// | models | deepseek-flash / deepseek-v4-pro | 「模型」一节 |
/// | 默认模型 | deepseek-flash | 脚注「模型名请使用 deepseek-flash」 |
/// | 支持工具 | 两者均支持 | 「Tool Calls」 |
/// | 支持图像 | 仅 deepseek-flash | 「图像理解」 |
class DeepSeekPreset {
  const DeepSeekPreset._();

  static const String name = 'DeepSeek';
  static const String baseUrl = 'https://api.deepseek.com';

  /// Anthropic 兼容端点（用户填这个时客户端只需如实传 base_url，协议由 Rust 自动解析）。
  static const String anthropicBaseUrl = 'https://api.deepseek.com/anthropic';

  /// 文档明确存在的两个模型（**不要往里加臆造的名字**）。
  static const List<String> models = <String>['deepseek-flash', 'deepseek-v4-pro'];

  /// 默认模型（文档脚注：模型名请使用 `deepseek-flash`）。
  static const String defaultModel = 'deepseek-flash';

  /// 旧名 → 新名（文档仍兼容调用；我们存新名以免 UI 出现过期值）。
  static const Map<String, String> legacyModels = <String, String>{
    'deepseek-v4-flash': 'deepseek-flash',
    'deepseek-v4-flash-vision-exp': 'deepseek-flash',
    // 本项目早期凭空写的名字（文档中不存在），统一迁到默认模型：
    'deepseek-chat': 'deepseek-flash',
    'deepseek-reasoner': 'deepseek-flash',
  };

  /// 是否 Anthropic 格式端点（仅用于提示，不改变协议选择）。
  static bool isAnthropicEndpoint(String url) => url.trim().endsWith('/anthropic');
}

/// 把旧模型名静默迁到新名；不需要迁移时返回原值（空值回落到默认模型）。
///
/// 返回记录里带 `migratedFrom`，便于诊断显示「已把旧模型名 X 迁移为 deepseek-flash」。
({String model, String? migratedFrom}) migrateModelName(String model) {
  final String trimmed = model.trim();
  final String? next = DeepSeekPreset.legacyModels[trimmed];
  if (next == null) {
    return (model: trimmed.isEmpty ? DeepSeekPreset.defaultModel : trimmed, migratedFrom: null);
  }
  return (model: next, migratedFrom: trimmed);
}

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
  /// **默认供应商**沿用老的 `byok_api_key`：升级后用户已保存的 Key 直接可用，不需要重填。
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

  /// 内置预设（值见 [DeepSeekPreset] 的文档注释）。
  List<ProviderConfig> _providers = const <ProviderConfig>[
    ProviderConfig(
      id: defaultProviderId,
      name: DeepSeekPreset.name,
      baseUrl: DeepSeekPreset.baseUrl,
      models: DeepSeekPreset.models,
    ),
  ];
  String _activeId = defaultProviderId;

  /// 旧模型名迁移记录（诊断用；为空 = 没迁过）。
  String? modelMigrationNote;
  final List<String> _migrationNotes = <String>[];

  List<ProviderConfig> get providers => List<ProviderConfig>.unmodifiable(_providers);
  String get activeId => _activeId;
  ProviderConfig get active => _providers.firstWhere(
        (ProviderConfig provider) => provider.id == _activeId,
        orElse: () => _providers.first,
      );

  /// 当前 provider 的模型列表（空则用内置预设的模型表）。
  List<String> get activeModels =>
      active.models.isEmpty ? DeepSeekPreset.models : active.models;

  /// 从设置里读回列表；没有则保持「DeepSeek 预设」。
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
      // t125：已存配置里的旧模型名静默迁移到新名（不打扰用户）。
      final List<ProviderConfig> migrated = <ProviderConfig>[];
      for (final ProviderConfig provider in parsed) {
        if (provider.models.isEmpty) {
          migrated.add(provider);
          continue;
        }
        final List<String> next = <String>[];
        for (final String model in provider.models) {
          final ({String model, String? migratedFrom}) result = migrateModelName(model);
          if (result.migratedFrom != null) {
            _migrationNotes.add('已把旧模型名 ${result.migratedFrom} 迁移为 ${result.model}');
          }
          if (!next.contains(result.model)) {
            next.add(result.model);
          }
        }
        migrated.add(provider.copyWith(models: next));
      }
      _providers = migrated;
      final String? active = decoded['active'] as String?;
      if (active != null && migrated.any((ProviderConfig p) => p.id == active)) {
        _activeId = active;
      }
      if (_migrationNotes.isNotEmpty) {
        modelMigrationNote = _migrationNotes.join('；');
        await _persist();
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
