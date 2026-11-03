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
    required this.onSettings,
    required this.onSignIn,
    required this.onSignOut,
    this.onDeleteKey,
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
  final VoidCallback onSettings;
  final VoidCallback onSignIn;
  final VoidCallback onSignOut;
  final VoidCallback? onDeleteKey;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 0),
      child: Row(
        children: <Widget>[
          SegmentedButton<String>(
            segments: const <ButtonSegment<String>>[
              ButtonSegment<String>(value: 'byok', label: Text('直连')),
              ButtonSegment<String>(value: 'cloud', label: Text('套餐')),
            ],
            selected: <String>{channel},
            showSelectedIcon: false,
            style: const ButtonStyle(
              visualDensity: VisualDensity.compact,
              tapTargetSize: MaterialTapTargetSize.shrinkWrap,
            ),
            onSelectionChanged: (Set<String> value) => onChannelChanged(value.first),
          ),
          const SizedBox(width: 4),
          // 状态：图标 + tooltip（不占横向空间，480px 也放得下）
          Tooltip(
            message: '$statusLabel\n搜索：$searchLabel\nKey：$keyLabel',
            child: Icon(
              keyDegraded ? Icons.warning_amber_outlined : Icons.info_outline,
              size: 20,
              color: keyDegraded ? theme.colorScheme.tertiary : theme.hintColor,
            ),
          ),
          const Spacer(),
          PopupMenuButton<String>(
            tooltip: '更多',
            icon: const Icon(Icons.more_vert),
            onSelected: (String value) {
              switch (value) {
                case 'local':
                  onLocalSearch();
                case 'memory':
                  onMemory();
                case 'settings':
                  onSettings();
                case 'signIn':
                  onSignIn();
                case 'signOut':
                  onSignOut();
                case 'deleteKey':
                  onDeleteKey?.call();
              }
            },
            itemBuilder: (BuildContext context) => <PopupMenuEntry<String>>[
              const PopupMenuItem<String>(value: 'local', child: Text('搜索本地资料（不联网）')),
              const PopupMenuItem<String>(value: 'memory', child: Text('记忆')),
              const PopupMenuItem<String>(value: 'settings', child: Text('设置')),
              if (keyLabel.isNotEmpty)
                const PopupMenuItem<String>(value: 'deleteKey', child: Text('删除已保存的 Key')),
              const PopupMenuDivider(),
              if (signedIn)
                PopupMenuItem<String>(value: 'signOut', child: Text('退出套餐（$accountLabel）'))
              else
                const PopupMenuItem<String>(value: 'signIn', child: Text('登录套餐账号')),
            ],
          ),
        ],
      ),
    );
  }
}
