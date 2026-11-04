// 对话页顶栏（独立文件）：**只放核心操作**，其余入口收进二级菜单。
//
// 为什么单独拆出来：`chat_page.dart` 已 1000+ 行，顶栏与业务状态耦合，
// 在 900+ 行文件里做结构替换反复出问题（t62 的教训）。这里保持纯展示：
// 所有状态与回调都由 `ChatPage` 传入，本文件不引用业务服务。
//
// 窄窗约束（t63 硬指标）：480×900 不得溢出。
// 做法：
// 1. 通道切换用短标签（「直连」/「套餐」），SegmentedButton 紧凑；
// 2. 状态文字不占宽度：改成 24px 的 info 图标 + `Tooltip`（整句文案在悬浮里）；
// 3. Key/搜索/记忆/设置/账号 全部进 `PopupMenuButton`（固定 48px）。

import 'package:flutter/material.dart';

/// 顶栏。
class ChatTopBar extends StatelessWidget {
  const ChatTopBar({
    super.key,
    required this.channel,
    required this.onChannelChanged,
    required this.statusLabel,
    required this.searchLabel,
    required this.keyLabel,
    required this.keyDegraded,
    required this.signedIn,
    required this.accountLabel,
    required this.onLocalSearch,
    required this.onMemory,
    required this.onDiagnostics,
    required this.onInfo,
    required this.onClearConversation,
  });

  final String channel;
  final ValueChanged<String> onChannelChanged;

  /// 一行状态（就绪/正在生成/未就绪原因），放 tooltip 里。
  final String statusLabel;
  final String searchLabel;
  final String keyLabel;
  final bool keyDegraded;
  final bool signedIn;
  final String accountLabel;

  final VoidCallback onLocalSearch;
  final VoidCallback onMemory;

  /// 对话诊断（**始终可达**：不是只在失败时出现）
  final VoidCallback onDiagnostics;

  /// t132：ⓘ 按钮 —— 显示状态 / 联网搜索来源 / Key 状态（原横幅内容收进来）。
  final VoidCallback onInfo;

  /// 清空当前对话历史（t112：二次确认在页面里做）
  final VoidCallback onClearConversation;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return _buildBar(context, theme);
  }

  Widget _buildBar(BuildContext context, ThemeData theme) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
      child: Row(
        children: <Widget>[
          // t132：顶栏不再放「直连/套餐」切换（与模型选择器里的「套餐通道」重复）。
          // 通道选择只保留在模型选择器弹层里；顶栏左侧用标题占位，右侧是 ⓘ 与 ⋮。
          Expanded(
            child: Align(
              alignment: Alignment.centerLeft,
              child: Text(
                statusLabel,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: theme.textTheme.bodySmall?.copyWith(color: theme.hintColor),
              ),
            ),
          ),
          const SizedBox(width: 4),
          // t132：搜索/Key/状态信息统一收进 ⓘ（不再占一整条横幅）。
          IconButton(
            key: const Key('chat-info'),
            tooltip: '状态与联网搜索',
            onPressed: onInfo,
            iconSize: 20,
            icon: Icon(
              keyDegraded ? Icons.warning_amber_outlined : Icons.info_outline,
              color: keyDegraded ? theme.colorScheme.tertiary : theme.hintColor,
            ),
          ),
          const SizedBox(width: 4),
          PopupMenuButton<String>(
            key: const Key('chat-menu'),
            tooltip: '更多',
            icon: const Icon(Icons.more_vert),
            onSelected: (String value) {
              switch (value) {
                case 'local':
                  onLocalSearch();
                case 'memory':
                  onMemory();
                case 'diagnostics':
                  onDiagnostics();
                case 'clear':
                  onClearConversation();
              }
            },
            itemBuilder: (BuildContext context) => <PopupMenuEntry<String>>[
              const PopupMenuItem<String>(value: 'local', child: Text('搜索本地资料（不联网）')),
              const PopupMenuItem<String>(value: 'memory', child: Text('记忆')),
              const PopupMenuItem<String>(value: 'diagnostics', child: Text('对话诊断')),
              const PopupMenuDivider(),
              const PopupMenuItem<String>(value: 'clear', child: Text('清空当前对话历史')),
            ],
          ),
        ],
      ),
    );
  }
}
