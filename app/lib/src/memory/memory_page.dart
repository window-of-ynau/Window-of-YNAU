// 记忆管理页（W7）：列表 / 新增 / 编辑 / 删除 + 到期过滤。
//
// 纯本地能力（ADR-19 原则 1）：不联网、不上云、不显示任何"同步"字样。

import 'package:flutter/material.dart';

import '../core/memory_service.dart';

/// 记忆管理页。
class MemoryPage extends StatefulWidget {
  const MemoryPage({super.key, required this.service});

  final MemoryService service;

  @override
  State<MemoryPage> createState() => _MemoryPageState();
}

class _MemoryPageState extends State<MemoryPage> {
  List<MemoryEntry> _entries = const <MemoryEntry>[];
  bool _loading = true;
  bool _hideExpired = true;
  MemoryKind? _kindFilter;
  String? _error;

  @override
  void initState() {
    super.initState();
    _reload();
  }

  Future<void> _reload() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final entries = await widget.service.list(kind: _kindFilter, hideExpired: _hideExpired);
      if (mounted) {
        setState(() => _entries = entries);
      }
    } on MemoryException catch (failure) {
      if (mounted) {
        setState(() => _error = failure.userHint);
      }
    } on Object {
      // 核心库不可用（测试环境等）：显示空态而不是崩
      if (mounted) {
        setState(() => _entries = const <MemoryEntry>[]);
      }
    } finally {
      if (mounted) {
        setState(() => _loading = false);
      }
    }
  }

  Future<void> _edit({MemoryEntry? entry}) async {
    final draft = await showDialog<_MemoryDraft>(
      context: context,
      builder: (BuildContext context) => _MemoryEditDialog(entry: entry),
    );
    if (draft == null) {
      return;
    }
    try {
      await widget.service.save(
        entry: entry,
        kind: draft.kind,
        title: draft.title,
        content: draft.content,
      );
      await _reload();
    } on MemoryException catch (failure) {
      if (mounted) {
        setState(() => _error = failure.userHint);
      }
    }
  }

  Future<void> _delete(MemoryEntry entry) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (BuildContext context) => AlertDialog(
        title: const Text('删除这条记忆？'),
        content: Text(entry.displayTitle),
        actions: <Widget>[
          TextButton(onPressed: () => Navigator.of(context).pop(false), child: const Text('取消')),
          FilledButton(onPressed: () => Navigator.of(context).pop(true), child: const Text('删除')),
        ],
      ),
    );
    if (confirmed != true) {
      return;
    }
    try {
      await widget.service.delete(entry);
      await _reload();
    } on MemoryException catch (failure) {
      if (mounted) {
        setState(() => _error = failure.userHint);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: const Text('记忆'),
        actions: <Widget>[
          IconButton(
            tooltip: '新增记忆',
            onPressed: () => _edit(),
            icon: const Icon(Icons.add),
          ),
        ],
      ),
      floatingActionButton: FloatingActionButton.extended(
        onPressed: () => _edit(),
        icon: const Icon(Icons.add),
        label: const Text('新增'),
      ),
      body: Column(
        children: <Widget>[
          Padding(
            padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
            child: Row(
              children: <Widget>[
                DropdownButton<MemoryKind?>(
                  value: _kindFilter,
                  hint: const Text('全部种类'),
                  items: <DropdownMenuItem<MemoryKind?>>[
                    const DropdownMenuItem<MemoryKind?>(value: null, child: Text('全部种类')),
                    for (final kind in MemoryKind.values)
                      DropdownMenuItem<MemoryKind?>(value: kind, child: Text(kind.label)),
                  ],
                  onChanged: (MemoryKind? value) {
                    setState(() => _kindFilter = value);
                    _reload();
                  },
                ),
                const SizedBox(width: 12),
                const Text('隐藏已到期'),
                Switch(
                  value: _hideExpired,
                  onChanged: (bool value) {
                    setState(() => _hideExpired = value);
                    _reload();
                  },
                ),
              ],
            ),
          ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
              child: Text(_error!, style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.error)),
            ),
          Expanded(
            child: _loading
                ? const Center(child: CircularProgressIndicator())
                : _entries.isEmpty
                    ? _EmptyState(onAdd: () => _edit())
                    : ListView.separated(
                        padding: const EdgeInsets.all(12),
                        itemCount: _entries.length,
                        separatorBuilder: (BuildContext context, int index) => const Divider(height: 1),
                        itemBuilder: (BuildContext context, int index) {
                          final entry = _entries[index];
                          return ListTile(
                            title: Row(
                              children: <Widget>[
                                Chip(
                                  label: Text(entry.kind.label),
                                  visualDensity: VisualDensity.compact,
                                ),
                                if (entry.expired) ...<Widget>[
                                  const SizedBox(width: 6),
                                  Chip(
                                    label: Text('已到期', style: theme.textTheme.labelSmall),
                                    visualDensity: VisualDensity.compact,
                                    backgroundColor: theme.colorScheme.surfaceContainerHighest,
                                  ),
                                ],
                                const SizedBox(width: 8),
                                Expanded(
                                  child: Text(entry.displayTitle, overflow: TextOverflow.ellipsis),
                                ),
                              ],
                            ),
                            subtitle: Text(entry.preview, maxLines: 2, overflow: TextOverflow.ellipsis),
                            trailing: Row(
                              mainAxisSize: MainAxisSize.min,
                              children: <Widget>[
                                IconButton(
                                  tooltip: '编辑',
                                  onPressed: () => _edit(entry: entry),
                                  icon: const Icon(Icons.edit_outlined, size: 20),
                                ),
                                IconButton(
                                  tooltip: '删除',
                                  onPressed: () => _delete(entry),
                                  icon: const Icon(Icons.delete_outline, size: 20),
                                ),
                              ],
                            ),
                            onTap: () => _edit(entry: entry),
                          );
                        },
                      ),
          ),
        ],
      ),
    );
  }
}

/// 空态：告诉用户这是**本机**记忆，不是云同步。
class _EmptyState extends StatelessWidget {
  const _EmptyState({required this.onAdd});

  final VoidCallback onAdd;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: <Widget>[
          const Icon(Icons.memory, size: 56),
          const SizedBox(height: 12),
          Text('还没有记忆', style: theme.textTheme.titleMedium),
          const SizedBox(height: 6),
          const Text('记下的内容只存在这台设备上，不会上传', textAlign: TextAlign.center),
          const SizedBox(height: 16),
          FilledButton.icon(
            onPressed: onAdd,
            icon: const Icon(Icons.add),
            label: const Text('新增一条'),
          ),
        ],
      ),
    );
  }
}

class _MemoryDraft {
  const _MemoryDraft({required this.kind, required this.title, required this.content});

  final MemoryKind kind;
  final String title;
  final String content;
}

class _MemoryEditDialog extends StatefulWidget {
  const _MemoryEditDialog({this.entry});

  final MemoryEntry? entry;

  @override
  State<_MemoryEditDialog> createState() => _MemoryEditDialogState();
}

class _MemoryEditDialogState extends State<_MemoryEditDialog> {
  late final TextEditingController _title =
      TextEditingController(text: widget.entry?.title ?? '');
  late final TextEditingController _content =
      TextEditingController(text: widget.entry?.content ?? '');
  late MemoryKind _kind = widget.entry?.kind ?? MemoryKind.note;

  @override
  void dispose() {
    _title.dispose();
    _content.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: Text(widget.entry == null ? '新增记忆' : '编辑记忆'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            DropdownButtonFormField<MemoryKind>(
              initialValue: _kind,
              decoration: const InputDecoration(labelText: '种类'),
              items: <DropdownMenuItem<MemoryKind>>[
                for (final kind in MemoryKind.values)
                  DropdownMenuItem<MemoryKind>(value: kind, child: Text(kind.label)),
              ],
              onChanged: (MemoryKind? value) => setState(() => _kind = value ?? _kind),
            ),
            TextField(
              controller: _title,
              decoration: const InputDecoration(labelText: '标题（可留空）'),
            ),
            TextField(
              controller: _content,
              minLines: 4,
              maxLines: 10,
              decoration: const InputDecoration(labelText: '内容'),
            ),
          ],
        ),
      ),
      actions: <Widget>[
        TextButton(onPressed: () => Navigator.of(context).pop(), child: const Text('取消')),
        FilledButton(
          onPressed: () {
            final content = _content.text.trim();
            if (content.isEmpty) {
              return;
            }
            Navigator.of(context).pop(_MemoryDraft(
              kind: _kind,
              title: _title.text.trim(),
              content: content,
            ));
          },
          child: const Text('保存'),
        ),
      ],
    );
  }
}
