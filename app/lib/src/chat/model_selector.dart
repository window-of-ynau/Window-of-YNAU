// 模型选择器（t123）：provider → 模型 → effort 三段弹层（两行带右侧值的菜单项）。
//
// 为什么放对话页：owner 的 IA 是「设置只管供应商；问什么/多努力属于这次对话」。
// 结构：
//   按钮（输入框上方左侧）= `provider · model · effort`
//   ↓ 点击
//   底部弹层：供应商 / 模型 / 推理等级 / 套餐通道（四行，右侧显示当前值 + ›）
//   ↓ 点某一行
//   二级弹层：该维度的候选列表（当前项打勾）

import 'package:flutter/material.dart';

import '../core/provider_store.dart';

class ModelSelector extends StatelessWidget {
  const ModelSelector({
    super.key,
    this.providers = const <ProviderConfig>[],
    this.activeProviderId,
    this.models = const <String>[],
    this.model = 'deepseek-chat',
    this.effort = 'auto',
    this.signedIn = false,
    this.accountLabel = '',
    this.onProviderChanged,
    this.onModelChanged,
    this.onEffortChanged,
    this.onUseCloud,
  });

  final List<ProviderConfig> providers;
  final String? activeProviderId;
  final List<String> models;
  final String model;
  final String effort;
  final bool signedIn;
  final String accountLabel;
  final ValueChanged<String>? onProviderChanged;
  final ValueChanged<String>? onModelChanged;
  final ValueChanged<String>? onEffortChanged;

  /// 「套餐通道」：未登录时由 ChatPage 弹登录框。
  final VoidCallback? onUseCloud;

  static const List<String> effortLabels = <String>['auto', 'low', 'medium', 'high'];

  static String effortText(String effort) {
    switch (effort) {
      case 'low':
        return '低';
      case 'medium':
        return '中';
      case 'high':
        return '高';
      default:
        return '自动';
    }
  }

  String get _providerName {
    for (final ProviderConfig provider in providers) {
      if (provider.id == activeProviderId) {
        return provider.name;
      }
    }
    return providers.isEmpty ? '未配置供应商' : providers.first.name;
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return Align(
      alignment: Alignment.centerLeft,
      child: TextButton.icon(
        key: const Key('model-selector-button'),
        onPressed: () => _openSheet(context),
        icon: const Icon(Icons.tune, size: 16),
        // 一行摘要：provider · 模型 · effort（窄窗靠 ellipsis，不溢出）
        label: Text(
          '$_providerName · $model · effort ${effortText(effort)}',
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: theme.textTheme.bodySmall,
        ),
        style: TextButton.styleFrom(
          padding: const EdgeInsets.symmetric(horizontal: 8),
          minimumSize: const Size(0, 32),
          tapTargetSize: MaterialTapTargetSize.shrinkWrap,
        ),
      ),
    );
  }

  Future<void> _openSheet(BuildContext context) async {
    await showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (BuildContext sheetContext) => SafeArea(
        child: ListView(
          shrinkWrap: true,
          children: <Widget>[
            _Row(
              title: '供应商',
              value: _providerName,
              onTap: providers.isEmpty
                  ? null
                  : () async {
                      final String? picked = await _pick(
                        sheetContext,
                        '供应商',
                        <_Option>[for (final ProviderConfig p in providers) _Option(p.id, p.name)],
                        activeProviderId ?? providers.first.id,
                      );
                      if (picked != null) {
                        onProviderChanged?.call(picked);
                      }
                    },
            ),
            _Row(
              title: '模型',
              value: model,
              onTap: models.isEmpty
                  ? null
                  : () async {
                      final String? picked = await _pick(
                        sheetContext,
                        '模型',
                        <_Option>[for (final String m in models) _Option(m, m)],
                        model,
                      );
                      if (picked != null) {
                        onModelChanged?.call(picked);
                      }
                    },
            ),
            _Row(
              title: '推理等级',
              value: effortText(effort),
              onTap: () async {
                final String? picked = await _pick(
                  sheetContext,
                  '推理等级',
                  <_Option>[for (final String e in effortLabels) _Option(e, effortText(e))],
                  effort,
                );
                if (picked != null) {
                  onEffortChanged?.call(picked);
                }
              },
            ),
            const Divider(height: 1),
            _Row(
              title: '套餐通道',
              value: signedIn ? (accountLabel.isEmpty ? '已登录' : accountLabel) : '未登录（点击登录）',
              onTap: onUseCloud,
            ),
          ],
        ),
      ),
    );
  }

  Future<String?> _pick(BuildContext context, String title, List<_Option> options, String current) {
    return showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (BuildContext inner) => SafeArea(
        child: ListView(
          shrinkWrap: true,
          children: <Widget>[
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
              child: Text(title, style: Theme.of(inner).textTheme.titleSmall),
            ),
            for (final _Option option in options)
              ListTile(
                dense: true,
                title: Text(option.label),
                trailing: option.value == current ? const Icon(Icons.check, size: 18) : null,
                onTap: () => Navigator.of(inner).pop(option.value),
              ),
          ],
        ),
      ),
    );
  }
}

class _Option {
  const _Option(this.value, this.label);

  final String value;
  final String label;
}

/// 两行式菜单项：左侧标题、右侧当前值 + `›`（与 owner 给的参考图一致）。
class _Row extends StatelessWidget {
  const _Row({required this.title, required this.value, this.onTap});

  final String title;
  final String value;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return ListTile(
      dense: true,
      title: Text(title),
      subtitle: Text(value, maxLines: 1, overflow: TextOverflow.ellipsis),
      trailing: Icon(Icons.chevron_right,
          size: 18, color: onTap == null ? theme.disabledColor : theme.hintColor),
      onTap: onTap,
    );
  }
}
