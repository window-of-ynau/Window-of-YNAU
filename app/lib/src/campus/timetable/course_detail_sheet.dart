// 课程详情：点课程块弹出的底部面板。
//
// 为什么是 bottom sheet 而不是对话框/悬浮卡：
// - 桌面上鼠标够得到、手机上拇指够得到；
// - 不需要为了"贴着被点的块"做定位计算，窗口宽窄都不出错；
// - 详情里的字段全是**已入库**数据（教师/教室/节次/周次），点开不触发任何取数。
//
// 缺字段不显示（"未知教室"这种占位比不显示更糟）：
// `room`/`teacher` 在 `StoredCourse` 里本来就是可空。

import 'package:flutter/material.dart';

import '../../core/timetable_service.dart';
import '../portal_api.dart';
import 'course_palette.dart';

/// 打开某门课的详情面板。
Future<void> showCourseDetail(
  BuildContext context, {
  required StoredCourse course,
  required int week,
  required Map<String, int> assignment,
}) {
  final CourseTint tint = courseTintOf(context, assignment, course.name);
  final Set<int> weeks = expandWeeks(course.weeks);
  final bool? inWeek = weeks.isEmpty ? null : weeks.contains(week);
  final String? room = (course.room ?? '').trim().isEmpty ? null : course.room!.trim();
  final String? teacher = (course.teacher ?? '').trim().isEmpty ? null : course.teacher!.trim();

  return showModalBottomSheet<void>(
    context: context,
    showDragHandle: true,
    isScrollControlled: true,
    builder: (BuildContext sheetContext) {
      final ThemeData theme = Theme.of(sheetContext);
      return SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 0, 20, 20),
          child: SingleChildScrollView(
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: <Widget>[
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: <Widget>[
                    Container(
                      width: 4,
                      height: 24,
                      margin: const EdgeInsets.only(top: 4, right: 10),
                      decoration: BoxDecoration(
                        color: tint.ink,
                        borderRadius: BorderRadius.circular(2),
                      ),
                    ),
                    Expanded(
                      child: Text(
                        course.name,
                        style: theme.textTheme.titleLarge?.copyWith(fontWeight: FontWeight.w600),
                      ),
                    ),
                    IconButton(
                      onPressed: () => Navigator.of(sheetContext).pop(),
                      icon: const Icon(Icons.close),
                      tooltip: '关闭',
                    ),
                  ],
                ),
                const SizedBox(height: 6),
                _DetailRow(label: '时间', value: '周${course.weekdayLabel} · ${course.periodLabel}'),
                if (room != null) _DetailRow(label: '地点', value: room),
                if (teacher != null) _DetailRow(label: '教师', value: teacher),
                _DetailRow(
                  label: '周次',
                  value: course.weeks.trim().isEmpty ? '未标注' : '第 ${course.weeks.trim()} 周',
                  trailing: inWeek == null
                      ? null
                      : _WeekBadge(inWeek: inWeek, tint: tint, theme: theme),
                ),
                const SizedBox(height: 10),
                Text(
                  '当前查看：第 $week 周　|　班级字段暂未入库（见课表可视化方案 §11）',
                  style: theme.textTheme.bodySmall?.copyWith(
                    color: theme.colorScheme.onSurfaceVariant.withValues(alpha: 0.8),
                  ),
                ),
              ],
            ),
          ),
        ),
      );
    },
  );
}

class _DetailRow extends StatelessWidget {
  const _DetailRow({required this.label, required this.value, this.trailing});

  final String label;
  final String value;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: <Widget>[
          SizedBox(
            width: 52,
            child: Text(
              label,
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant.withValues(alpha: 0.7),
              ),
            ),
          ),
          Expanded(
            child: Text(
              value,
              style: theme.textTheme.bodyMedium,
            ),
          ),
          if (trailing != null) trailing!,
        ],
      ),
    );
  }
}

class _WeekBadge extends StatelessWidget {
  const _WeekBadge({required this.inWeek, required this.tint, required this.theme});

  final bool inWeek;
  final CourseTint tint;
  final ThemeData theme;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        color: inWeek ? tint.container : theme.colorScheme.surfaceContainerHighest,
        borderRadius: BorderRadius.circular(99),
        border: Border.all(color: inWeek ? tint.border : theme.colorScheme.outlineVariant),
      ),
      child: Text(
        inWeek ? '本周在课' : '本周不上',
        style: theme.textTheme.labelSmall?.copyWith(
          color: inWeek ? tint.ink : theme.colorScheme.onSurfaceVariant,
        ),
      ),
    );
  }
}
