// 课表的两类"提示位"：
//   1. 未确认学期开始日（琥珀色横幅，**与"有课/无课"正交**：课表照常显示，只是不标日期）；
//   2. 本周无课（中性灰蓝卡片，绝不红/琥珀——它不是错误）。
//
// 解析失败的红卡片仍在 `timetable_page.dart` 里（沿用既有契约），三者颜色家族不同，
// 灰度打印或色弱也能靠图标形状分辨。

import 'package:flutter/material.dart';

/// 琥珀色（浅色主题）：确认类提示。对比度 8.6:1。
const Color _kAmberBg = Color(0xFFFFF3CD);
const Color _kAmberFg = Color(0xFF5C4400);
const Color _kAmberLine = Color(0xFFE8CE85);

/// 琥珀色（深色主题）：低明度底 + 高明度字。
const Color _kAmberBgDark = Color(0xFF3A2F12);
const Color _kAmberFgDark = Color(0xFFF0DFA8);
const Color _kAmberLineDark = Color(0xFF6B5623);

/// 「还没确认学期开始日」横幅。
///
/// 文案分两层：**加粗那行说"要你做什么"**，小字说"为什么/影响是什么"。
/// 两种入口（同一个组件）：
/// - 该学期没有可靠建议值 → 只给「选择开学日」；
/// - 有校历建议值但用户还没确认 → 多给一个「确认这一天」的快捷动作。
class TermStartPrompt extends StatelessWidget {
  const TermStartPrompt({
    super.key,
    required this.title,
    required this.subtitle,
    required this.onPickDate,
    this.suggestedLabel,
    this.onUseSuggested,
  });

  final String title;
  final String subtitle;

  /// 建议值按钮的文案（如「确认 09-07」）；`null` = 不给这个按钮。
  final String? suggestedLabel;
  final VoidCallback? onUseSuggested;
  final VoidCallback onPickDate;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final bool dark = theme.brightness == Brightness.dark;
    final Color bg = dark ? _kAmberBgDark : _kAmberBg;
    final Color fg = dark ? _kAmberFgDark : _kAmberFg;
    final Color line = dark ? _kAmberLineDark : _kAmberLine;

    return Container(
      margin: const EdgeInsets.fromLTRB(12, 6, 12, 0),
      padding: const EdgeInsets.fromLTRB(12, 10, 12, 10),
      decoration: BoxDecoration(
        color: bg,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: line),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: <Widget>[
          Icon(Icons.info_outline, size: 18, color: fg),
          const SizedBox(width: 8),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: <Widget>[
                Text(
                  title,
                  style: theme.textTheme.bodyMedium?.copyWith(
                    color: fg,
                    fontWeight: FontWeight.w700,
                  ),
                ),
                const SizedBox(height: 2),
                Text(
                  subtitle,
                  style: theme.textTheme.bodySmall?.copyWith(color: fg.withValues(alpha: 0.88)),
                ),
                const SizedBox(height: 8),
                // Wrap：窄窗口下按钮自己换行，不挤成溢出
                Wrap(
                  spacing: 8,
                  runSpacing: 6,
                  children: <Widget>[
                    OutlinedButton(
                      onPressed: onPickDate,
                      style: OutlinedButton.styleFrom(
                        foregroundColor: fg,
                        side: BorderSide(color: line),
                        padding: const EdgeInsets.symmetric(horizontal: 12),
                        minimumSize: const Size(0, 34),
                      ),
                      child: const Text('选择开学日'),
                    ),
                    if (suggestedLabel != null && onUseSuggested != null)
                      FilledButton(
                        onPressed: onUseSuggested,
                        style: FilledButton.styleFrom(
                          backgroundColor: fg,
                          foregroundColor: bg,
                          padding: const EdgeInsets.symmetric(horizontal: 12),
                          minimumSize: const Size(0, 34),
                        ),
                        child: Text(suggestedLabel!),
                      ),
                  ],
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// 「第 N 周没有课」：中性空态。
///
/// 关键差别：它**不是**"还没有导入课表"（那是有没有数据的问题），
/// 也**不是**解析失败（那是红色错误）。这里给的是"本周确实没排课"，
/// 所以直接给相邻有课的周做跳板，而不是让用户自己一周周翻。
class WeekEmptyCard extends StatelessWidget {
  const WeekEmptyCard({
    super.key,
    required this.week,
    required this.totalCourses,
    required this.weekCount,
    required this.nearestWeeks,
    required this.onSelectWeek,
    required this.canPrevious,
    required this.canNext,
    required this.onPreviousWeek,
    required this.onNextWeek,
  });

  final int week;
  final int totalCourses;
  final int? weekCount;

  /// 相邻的、确实有课的周号（最多两个）。
  final List<int> nearestWeeks;
  final void Function(int week) onSelectWeek;
  final bool canPrevious;
  final bool canNext;
  final VoidCallback onPreviousWeek;
  final VoidCallback onNextWeek;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final ColorScheme colors = theme.colorScheme;
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            Container(
              width: 72,
              height: 72,
              decoration: BoxDecoration(
                color: colors.surfaceContainerHighest.withValues(alpha: 0.6),
                borderRadius: BorderRadius.circular(22),
              ),
              child: Icon(Icons.event_busy_outlined, size: 32, color: colors.onSurfaceVariant),
            ),
            const SizedBox(height: 14),
            Text('第 $week 周没有课', style: theme.textTheme.titleMedium),
            const SizedBox(height: 6),
            Text(
              totalCourses == 0
                  ? '本学期还没有排课数据。'
                  : '这学期共 $totalCourses 门课，本周无排课。试试左右切换周次。',
              textAlign: TextAlign.center,
              style: theme.textTheme.bodySmall?.copyWith(color: colors.onSurfaceVariant),
            ),
            if (weekCount != null) ...<Widget>[
              const SizedBox(height: 4),
              Text(
                '共 $weekCount 周',
                style: theme.textTheme.bodySmall?.copyWith(
                  color: colors.onSurfaceVariant.withValues(alpha: 0.7),
                ),
              ),
            ],
            const SizedBox(height: 14),
            Wrap(
              spacing: 8,
              runSpacing: 6,
              alignment: WrapAlignment.center,
              children: <Widget>[
                OutlinedButton(
                  onPressed: canPrevious ? onPreviousWeek : null,
                  style: OutlinedButton.styleFrom(minimumSize: const Size(0, 34)),
                  child: const Text('上一周'),
                ),
                for (final int near in nearestWeeks)
                  FilledButton.tonal(
                    onPressed: () => onSelectWeek(near),
                    style: FilledButton.styleFrom(minimumSize: const Size(0, 34)),
                    child: Text('第 $near 周 · 有课'),
                  ),
                OutlinedButton(
                  onPressed: canNext ? onNextWeek : null,
                  style: OutlinedButton.styleFrom(minimumSize: const Size(0, 34)),
                  child: const Text('下一周'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
