// 设置页「供应商」卡片（t115）：唯一能填 Key 的地方。
//
// 协议**不可选**：由 Rust 侧按 provider 自动解析（responses → messages → completions），
// 这里只显示「当前生效：自动」。

import 'package:flutter/material.dart';

import '../core/provider_store.dart';
import '../core/secret_service.dart';

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
      builder: (BuildContext context) => _ProviderDialog(provider: provider),
    );
    if (draft == null || !mounted) {
      return;
    }
    setState(() => _busy = true);
    try {
      final String id = provider?.id ?? DateTime.now().millisecondsSinceEpoch.toRadixString(36);
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
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('保存失败：$failure')));
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
        title: Text('删除供应商「${provider.name}」？'),
        content: const Text('只删除本机保存的配置与 Key，不影响账号。'),
        actions: <Widget>[
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.pop(context, true), child: const Text('删除')),
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
      return '${provider.baseUrl} · 端侧核心不可用';
    }
    return '${provider.baseUrl} · ${hasKey ? (status?.broken ?? false ? 'Key 密文损坏，请重填' : 'Key 已保存') : '未配置 Key'}';
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
                    tooltip: '编辑',
                    onPressed: _busy ? null : () => _edit(provider: provider),
                    icon: const Icon(Icons.edit_outlined, size: 18),
                  ),
                  IconButton(
                    tooltip: '删除',
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
                    protocolAutoLabel(widget.store.active.name),
                    style: theme.textTheme.bodySmall?.copyWith(color: theme.hintColor),
                  ),
                ),
                TextButton.icon(
                  onPressed: _busy ? null : () => _edit(),
                  icon: const Icon(Icons.add, size: 18),
                  label: const Text('添加供应商'),
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
  });

  final String name;
  final String baseUrl;
  final String apiKey;
  final List<String> models;
}

class _ProviderDialog extends StatefulWidget {
  const _ProviderDialog({this.provider});

  final ProviderConfig? provider;

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

  @override
  void dispose() {
    _name.dispose();
    _baseUrl.dispose();
    _key.dispose();
    _models.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: Text(widget.provider == null ? '添加供应商' : '编辑供应商'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            TextField(controller: _name, decoration: const InputDecoration(labelText: '名称')),
            TextField(controller: _baseUrl, decoration: const InputDecoration(labelText: 'Base URL')),
            TextField(
              controller: _key,
              obscureText: true,
              decoration: InputDecoration(
                labelText: 'API Key',
                hintText: widget.provider == null ? '' : '已保存则留空不改',
              ),
            ),
            TextField(
              controller: _models,
              decoration: const InputDecoration(
                labelText: '自定义模型（可选，逗号分隔）',
              ),
            ),
          ],
        ),
      ),
      actions: <Widget>[
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('取消')),
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
            Navigator.pop(
              context,
              _ProviderDraft(
                name: name,
                baseUrl: baseUrl,
                apiKey: _key.text.trim(),
                models: models,
              ),
            );
          },
          child: const Text('保存'),
        ),
      ],
    );
  }
}
