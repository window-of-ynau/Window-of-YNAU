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
import '../theme/yunong_tokens.dart';

class ModelSelector extends StatelessWidget {
  const ModelSelector({
    super.key,
    this.providers = const <ProviderConfig>[],
    this.activeProviderId,
    this.models = const <String>[],
    this.model = DeepSeekPreset.defaultModel,
    this.effort = 'auto',
    this.signedIn = false,
    this.accountLabel = '',
    this.onProviderChanged,
    this.onModelChanged,
    this.onEffortChanged,
    this.onUseCloud,
    this.onPresetSelected,
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

  /// 【t168】选中一个内置预设（= 登记成 Provider 配置并设为当前）。
  final ValueChanged<ProviderPreset>? onPresetSelected;

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
    return providers.isEmpty ? ProviderStore.placeholderLabel : providers.first.name;
  }

  /// t172：composer 里的「文字 + 折角」样式（owner 图二）：
  /// `DeepSeek V4.1 Flash  Default ⌄` —— 纯文字，无填充、无边框。
  String get _summary {
    if (providers.isEmpty) {
      return ProviderStore.placeholderLabel;
    }
    // 名字与 effort 之间留两个空格（owner 的图里就是这样排的）
    return '$_providerName  ${effortText(effort)}';
  }

  /// t174：窄屏只保留**模型名**（没有配置时直接不显示文字），把宽度让给输入框。
  String get _compactSummary {
    if (providers.isEmpty) {
      return '';
    }
    final String name = model.trim().isEmpty ? _providerName : model.trim();
    return name;
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final YunongTokens tokens = context.tokens;
    // t174：窄屏（<600）**只留模型名 + ⌄**（未配置就只留 ⌄），宽度让给输入框。
    final bool compact = MediaQuery.sizeOf(context).width < 600;
    final String compactText = _compactSummary;
    // 「文字 + 折角」：无背景、无边框，整体可点（打开三段弹层）。
    return ConstrainedBox(
      // t174：窄屏把宽度压到最小（未配置=只留 ⌄=36；已配置=模型名 ≤96），把空间全让给输入框。
      constraints: BoxConstraints(maxWidth: compact ? (providers.isEmpty ? 36 : 96) : 210),
      child: TextButton(
        key: const Key('model-selector-button'),
        onPressed: () => _openSheet(context),
        style: TextButton.styleFrom(
          padding: EdgeInsets.symmetric(horizontal: compact ? 2 : 4),
          minimumSize: const Size(0, 32),
          tapTargetSize: MaterialTapTargetSize.shrinkWrap,
          overlayColor: Colors.transparent,
          backgroundColor: Colors.transparent,
          foregroundColor: tokens.inkSoft,
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            if (!compact || compactText.isNotEmpty)
              Flexible(
                child: Text(
                  compact ? compactText : _summary,
                  key: const Key('model-selector-summary'),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: providers.isEmpty ? theme.hintColor : tokens.inkSoft,
                  ),
                ),
              ),
            const SizedBox(width: 2),
            Icon(Icons.expand_more, size: 16, color: providers.isEmpty ? theme.hintColor : tokens.inkSoft),
          ],
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
              // t168：即使一个都没配置，也允许打开（里面是"预设模板"）
              onTap: () async {
                final String? picked = await _pick(
                  sheetContext,
                  '供应商',
                  <_Option>[
                    for (final ProviderConfig p in providers)
                      _Option('provider:${p.id}', p.name),
                    for (final ProviderPreset preset in ProviderPreset.all)
                      if (!providers.any((ProviderConfig p) => p.id == 'preset-${preset.id}'))
                        _Option('preset:${preset.id}', '${preset.displayName}（预设）'),
                  ],
                  activeProviderId == null ? '' : 'provider:$activeProviderId',
                );
                if (picked == null) {
                  return;
                }
                if (picked.startsWith('preset:')) {
                  final String id = picked.substring('preset:'.length);
                  for (final ProviderPreset preset in ProviderPreset.all) {
                    if (preset.id == id) {
                      onPresetSelected?.call(preset);
                      return;
                    }
                  }
                  return;
                }
                onProviderChanged?.call(picked.substring('provider:'.length));
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
