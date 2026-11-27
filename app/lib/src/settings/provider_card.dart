// 设置页「供应商」卡片（t115）：唯一能填 Key 的地方。
//
// 协议**不可选**：由 Rust 侧按 provider 自动解析（responses → messages → completions），
// 这里只显示「当前生效：自动」。

import 'package:flutter/material.dart';

import '../core/provider_store.dart';
import '../core/secret_service.dart';
import '../l10n/strings.dart';

class ProviderCard extends StatefulWidget {
  const ProviderCard({super.key, required this.store, required this.secrets});

  final ProviderStore store;
  final SecretService? secrets;

  @override
  State<ProviderCard> createState() => _ProviderCardState();
}

class _ProviderCardState extends State<ProviderCard> {
  final Map<String, SecretStatus> _status = <String, SecretStatus>{};
  bool _legacyKey = false;
  bool _busy = false;

  @override
  void initState() {
    super.initState();
    widget.store.addListener(_onStoreChanged);
    _refreshKeys();
  }

  @override
  void dispose() {
    widget.store.removeListener(_onStoreChanged);
    super.dispose();
  }

  void _onStoreChanged() {
    if (mounted) {
      setState(() {});
    }
  }

  Future<void> _refreshKeys() async {
    final SecretService? secrets = widget.secrets;
    if (secrets == null) {
      return;
    }
    _legacyKey = await widget.store.legacyKeyAvailable();
    for (final ProviderConfig provider in widget.store.providers) {
      try {
        _status[provider.id] = await secrets.status(provider.secretKeyName);
      } on Object {
        // 读不到就不显示状态，不影响其它 provider。
      }
    }
    if (mounted) {
      setState(() {});
    }
  }

  Future<void> _edit({ProviderConfig? provider}) async {
    final _ProviderDraft? draft = await showDialog<_ProviderDraft>(
      context: context,
      builder: (BuildContext context) =>
          _ProviderDialog(provider: provider, store: widget.store),
    );
    if (draft == null || !mounted) {
      return;
    }
    setState(() => _busy = true);
    try {
      // #77：从内置预设进来的沿用 `preset-<id>`（这样「已添加」判断/Key 名都稳定）；
      // 自定义走原来的时间戳 id。
      final String id =
          provider?.id ?? draft.id ?? DateTime.now().millisecondsSinceEpoch.toRadixString(36);
      final ProviderConfig next = ProviderConfig(
        id: id,
        name: draft.name,
        baseUrl: draft.baseUrl,
        models: draft.models,
      );
      if (provider == null) {
        await widget.store.add(next);
      } else {
        await widget.store.update(next);
      }
      if (draft.apiKey.isNotEmpty) {
        await widget.secrets?.save(next.secretKeyName, draft.apiKey);
      }
      await _refreshKeys();
    } on Object catch (failure) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
          SnackBar(content: Text(SettingsS.saveFailed(failure))),
        );
      }
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  Future<void> _delete(ProviderConfig provider) async {
    final bool? ok = await showDialog<bool>(
      context: context,
      builder: (BuildContext context) => AlertDialog(
        title: Text(SettingsS.deleteProviderTitle(provider.name)),
        content: const Text(SettingsS.deleteConfirmBody),
        actions: <Widget>[
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text(SettingsS.cancel),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text(SettingsS.delete),
          ),
        ],
      ),
    );
    if (ok != true) {
      return;
    }
    await widget.store.remove(provider.id);
    await widget.secrets?.delete(provider.secretKeyName);
    await _refreshKeys();
  }

  String _subtitleFor(ProviderConfig provider) {
    final SecretStatus? status = _status[provider.id];
    final bool hasKey = (status?.set ?? false) ||
        (provider.id == ProviderStore.defaultProviderId && _legacyKey);
    if (widget.secrets == null) {
      return SettingsS.providerSubtitleCoreUnavailable(provider.baseUrl);
    }
    return SettingsS.providerSubtitle(
      provider.baseUrl,
      hasKey: hasKey,
      broken: status?.broken ?? false,
    );
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final List<ProviderConfig> providers = widget.store.providers;
    return Material(
      type: MaterialType.transparency,
      child: Column(
        children: <Widget>[
          for (final ProviderConfig provider in providers)
            RadioListTile<String>(
              value: provider.id,
              // ignore: deprecated_member_use
              groupValue: widget.store.activeId,
              // ignore: deprecated_member_use
              onChanged: _busy ? null : (String? id) => widget.store.setActive(id ?? provider.id),
              title: Text(provider.name),
              subtitle: Text(_subtitleFor(provider), maxLines: 1, overflow: TextOverflow.ellipsis),
              secondary: Row(
                mainAxisSize: MainAxisSize.min,
                children: <Widget>[
                  IconButton(
                    tooltip: SettingsS.edit,
                    onPressed: _busy ? null : () => _edit(provider: provider),
                    icon: const Icon(Icons.edit_outlined, size: 18),
                  ),
                  IconButton(
                    tooltip: SettingsS.delete,
                    onPressed: _busy || providers.length <= 1 ? null : () => _delete(provider),
                    icon: const Icon(Icons.delete_outline, size: 18),
                  ),
                ],
              ),
            ),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
            child: Row(
              children: <Widget>[
                Expanded(
                  child: Text(
                    // 协议不可选：Rust 侧自动解析（responses → messages → completions）
                    protocolAutoLabel(widget.store.activeOrNull?.name ?? ProviderStore.placeholderLabel),
                    style: theme.textTheme.bodySmall?.copyWith(color: theme.hintColor),
                  ),
                ),
                TextButton.icon(
                  onPressed: _busy ? null : () => _edit(),
                  icon: const Icon(Icons.add, size: 18),
                  label: const Text(SettingsS.addProvider),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _ProviderDraft {
  const _ProviderDraft({
    required this.name,
    required this.baseUrl,
    required this.apiKey,
    required this.models,
    this.id,
  });

  final String name;
  final String baseUrl;
  final String apiKey;
  final List<String> models;

  /// 非空 = 从内置预设填出来的配置，沿用稳定 id（`preset-<预设id>`）。
  final String? id;
}

class _ProviderDialog extends StatefulWidget {
  const _ProviderDialog({this.provider, required this.store});

  final ProviderConfig? provider;

  /// #77：用来判断哪些内置预设已经添加过（标「已添加」）。
  final ProviderStore store;

  @override
  State<_ProviderDialog> createState() => _ProviderDialogState();
}

class _ProviderDialogState extends State<_ProviderDialog> {
  late final TextEditingController _name =
      TextEditingController(text: widget.provider?.name ?? '');
  late final TextEditingController _baseUrl =
      TextEditingController(text: widget.provider?.baseUrl ?? 'https://');
  final TextEditingController _key = TextEditingController();
  late final TextEditingController _models =
      TextEditingController(text: (widget.provider?.models ?? const <String>[]).join(', '));
  final FocusNode _keyFocus = FocusNode();

  /// #77：当前从哪个内置预设填的（null = 自定义）。
  String? _selectedPresetId;

  @override
  void dispose() {
    _name.dispose();
    _baseUrl.dispose();
    _key.dispose();
    _models.dispose();
    _keyFocus.dispose();
    super.dispose();
  }

  /// #77：点选预设 → 填 name/baseUrl/modelId，只留 API Key 给用户补。
  void _applyPreset(ProviderPreset preset) {
    setState(() {
      _selectedPresetId = preset.id;
      _name.text = preset.displayName;
      _baseUrl.text = preset.baseUrl;
      _models.text = preset.modelId;
    });
    _keyFocus.requestFocus();
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return AlertDialog(
      key: const Key('provider-dialog'),
      title: Text(widget.provider == null ? SettingsS.addProvider : SettingsS.editProvider),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            // #77：内置预设只在「添加」时出现；编辑已有配置不显示，避免误解为改名。
            if (widget.provider == null) ...<Widget>[
              Align(
                alignment: Alignment.centerLeft,
                child: Text(SettingsS.builtinPresets, style: theme.textTheme.labelLarge),
              ),
              const SizedBox(height: 4),
              for (final ProviderPreset preset in ProviderPreset.all)
                _presetTile(theme, preset),
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 6),
                child: Divider(height: 1, color: theme.dividerColor),
              ),
              Align(
                alignment: Alignment.centerLeft,
                child: Text(SettingsS.orCustom, style: theme.textTheme.labelLarge),
              ),
              const SizedBox(height: 4),
            ],
            TextField(
              key: const Key('provider-name'),
              controller: _name,
              decoration: const InputDecoration(labelText: SettingsS.fieldName),
            ),
            TextField(
              key: const Key('provider-base-url'),
              controller: _baseUrl,
              decoration: const InputDecoration(labelText: SettingsS.fieldBaseUrl),
            ),
            TextField(
              key: const Key('provider-api-key'),
              controller: _key,
              focusNode: _keyFocus,
              obscureText: true,
              decoration: InputDecoration(
                labelText: SettingsS.fieldApiKey,
                hintText: widget.provider == null ? '' : SettingsS.keepBlankIfSaved,
              ),
            ),
            TextField(
              key: const Key('provider-models'),
              controller: _models,
              decoration: const InputDecoration(
                labelText: SettingsS.fieldModels,
              ),
            ),
          ],
        ),
      ),
      actions: <Widget>[
        TextButton(onPressed: () => Navigator.pop(context), child: const Text(SettingsS.cancel)),
        FilledButton(
          onPressed: () {
            final String name = _name.text.trim();
            final String baseUrl = _baseUrl.text.trim();
            if (name.isEmpty || baseUrl.isEmpty) {
              return;
            }
            final List<String> models = _models.text
                .split(',')
                .map((String item) => item.trim())
                .where((String item) => item.isNotEmpty)
                .toList();
            // 只有仍与原预设一致时才认作「该预设」（用户改了名字/地址 = 自定义）。
            String? presetId;
            final String? selected = _selectedPresetId;
            if (selected != null) {
              final ProviderPreset preset =
                  ProviderPreset.all.firstWhere((ProviderPreset item) => item.id == selected);
              final bool unchanged = name == preset.displayName &&
                  baseUrl == preset.baseUrl &&
                  models.length == 1 &&
                  models.first == preset.modelId;
              if (unchanged) {
                presetId = 'preset-${preset.id}';
              }
            }
            Navigator.pop(
              context,
              _ProviderDraft(
                id: presetId,
                name: name,
                baseUrl: baseUrl,
                apiKey: _key.text.trim(),
                models: models,
              ),
            );
          },
          child: const Text(SettingsS.save),
        ),
      ],
    );
  }

  /// 一个内置预设行：显示名 + baseUrl；已添加的置灰并标「已添加」。
  Widget _presetTile(ThemeData theme, ProviderPreset preset) {
    final String configId = 'preset-${preset.id}';
    final bool added = widget.store.hasProvider(configId);
    final bool selected = !added && _selectedPresetId == preset.id;
    return ListTile(
      key: Key('preset-${preset.id}'),
      dense: true,
      contentPadding: EdgeInsets.zero,
      enabled: !added,
      onTap: added ? null : () => _applyPreset(preset),
      leading: Icon(
        selected ? Icons.check_circle : Icons.bolt_outlined,
        size: 20,
        color: added ? theme.disabledColor : (selected ? theme.colorScheme.primary : null),
      ),
      title: Text(
        preset.displayName,
        style: theme.textTheme.bodyMedium?.copyWith(
          color: added ? theme.disabledColor : null,
          fontWeight: FontWeight.w500,
        ),
      ),
      subtitle: Text(preset.baseUrl, maxLines: 1, overflow: TextOverflow.ellipsis),
      trailing: added
          ? Text(
              SettingsS.alreadyAdded,
              key: Key('preset-added-${preset.id}'),
              style: theme.textTheme.bodySmall?.copyWith(color: theme.disabledColor),
            )
          : (selected
              ? Text(SettingsS.selected,
                  style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.primary))
              : null),
    );
  }
}
