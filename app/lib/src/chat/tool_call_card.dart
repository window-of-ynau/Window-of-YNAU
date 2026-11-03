// 工具调用卡片（t102）：对话里呈现"模型调用了什么工具、参数是什么、拿到什么结果"。
//
// 设计要点：
// - **默认收起**：不要把大段 JSON 塞进对话流（参数与结果都折叠）；
// - 失败必须显式：超时/被拒绝/参数错都显示为红色状态行，不静默；
// - 纯展示组件：状态由 `ChatBubble` 传入，便于单测。

import 'package:flutter/material.dart';

/// 工具调用状态。
enum ToolCallStatus { running, ok, failed, cancelled }

/// 工具调用卡片。
class ToolCallCard extends StatefulWidget {
  const ToolCallCard({
    super.key,
    required this.toolName,
    required this.status,
    this.args,
    this.summary,
    this.errorText,
  });

  /// 工具名（如 `timetable_query`）；UI 会加中文别名。
  final String toolName;
  final ToolCallStatus status;

  /// 参数 JSON 原文（折叠展示）。
  final String? args;

  /// 结果摘要（如「5 条结果」）。
  final String? summary;

  /// 失败原因（用户可读）。
  final String? errorText;

  @override
  State<ToolCallCard> createState() => _ToolCallCardState();
}

class _ToolCallCardState extends State<ToolCallCard> {
  bool _expanded = false;

  String get _label {
    switch (widget.toolName) {
      case 'timetable_query':
        return '课表查询';
      case 'score_query':
        return '成绩查询';
      default:
        return widget.toolName;
    }
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final ColorScheme scheme = theme.colorScheme;
    final bool failed = widget.status == ToolCallStatus.failed;
    final bool running = widget.status == ToolCallStatus.running;
    // t104：取消 ≠ 失败——用户主动停止，用中性色而不是红色错误
    final bool cancelled = widget.status == ToolCallStatus.cancelled;

    final Color statusColor = failed
        ? scheme.error
        : (running ? scheme.primary : scheme.onSurfaceVariant);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 4),
      child: Material(
        // 实心背景：与对话区其它卡片一致，玻璃主题下也不透底
        color: scheme.surfaceContainerHigh,
        borderRadius: BorderRadius.circular(10),
        child: InkWell(
          borderRadius: BorderRadius.circular(10),
          onTap: () => setState(() => _expanded = !_expanded),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: <Widget>[
                Row(
                  children: <Widget>[
                    Icon(
                      failed ? Icons.error_outline : (running ? Icons.hourglass_top : Icons.build_outlined),
                      size: 16,
                      color: statusColor,
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        '调用 ${_label}${_argsHint()}',
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurface,
                          fontWeight: FontWeight.w600,
                        ),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                    Text(
                      cancelled
                          ? '已取消'
                          : failed
                              ? '失败'
                              : running
                                  ? '进行中…'
                                  : (widget.summary ?? '已完成'),
                      style: theme.textTheme.bodySmall?.copyWith(color: statusColor),
                    ),
                    Icon(_expanded ? Icons.expand_less : Icons.expand_more, size: 18, color: scheme.onSurfaceVariant),
                  ],
                ),
                if ((failed || cancelled) && widget.errorText != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 4, left: 24),
                    child: Text(
                      widget.errorText!,
                      style: theme.textTheme.bodySmall?.copyWith(color: failed ? scheme.error : scheme.onSurfaceVariant),
                    ),
                  ),
                if (_expanded && widget.args != null && widget.args!.isNotEmpty)
                  Padding(
                    padding: const EdgeInsets.only(top: 6, left: 24),
                    child: SelectableText(
                      widget.args!,
                      style: theme.textTheme.bodySmall?.copyWith(
                        fontFamily: 'monospace',
                        color: scheme.onSurfaceVariant,
                      ),
                    ),
                  ),
              ],
            ),
          ),
        ),
      ),
    );
  }

  /// 参数里的简短提示（如 `week=8`）：只挑简单标量，不把整段 JSON 塞进标题。
  String _argsHint() {
    final String? args = widget.args;
    if (args == null || args.isEmpty) {
      return '';
    }
    final RegExpMatch? match = RegExp(r'"?(\w+)"?\s*[:=]\s*"?([\w\-]+)"?').firstMatch(args);
    if (match == null) {
      return '';
    }
    return '(${match.group(1)}=${match.group(2)})';
  }
}

/// 工具失败码 → 中文可读文案（t104）。
///
/// Rust 侧会返回 `TIMEOUT / BAD_ARGS / NOT_ALLOWED / CANCELLED / ROUND_LIMIT / TOOL_ERROR`；
/// **不要把英文码直接显示给用户**。`CANCELLED` 单独文案（用户主动停止，不是失败）。
String toolFailureText(String? code, {String? fallback}) {
  switch (code) {
    case 'TIMEOUT':
      return '工具执行超时，已放弃本次结果';
    case 'BAD_ARGS':
      return '工具参数不合法，本次调用未执行';
    case 'NOT_ALLOWED':
      return '该工具未获授权，已拒绝调用';
    case 'CANCELLED':
      return '已取消（你点了停止）';
    case 'ROUND_LIMIT':
      return '本轮工具调用次数已达上限';
    case 'TOOL_ERROR':
      return '工具执行出错';
    default:
      return fallback ?? '工具调用失败';
  }
}