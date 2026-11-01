// 课表网格：7 天（或折叠后的 5 天）× 10 节的真实网格。
//
// 只负责**渲染与交互**：取数、解析、合并、入库都在 Rust 侧，周次过滤在调用方。
// 三条硬约束（都是踩过的坑）：
//   1. 不溢出：列宽/行高全部由 `LayoutBuilder` 的实际约束算出，**没有固定宽度拼 Row**；
//      块内文字按可用高度决定显示几行，任何高度都不会撑破块；
//   2. 整周可见优先：行高在 48~60 之间自适应，1280×720 的默认窗口正好放下 10 节；
//   3. 不猜数据：没有学期开始日就不显示日期、不高亮"今天"。

import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../../core/timetable_service.dart';
import 'course_detail_sheet.dart';
import 'course_palette.dart';

/// 一天固定 10 节（与门户 `queryAWeekSchedule` 一致）。
const int kPeriodCount = 10;

/// 表头（星期/日期）高度。
const double kGridHeaderHeight = 44;

/// 行高自适应区间：下限保证单节块还能放"课名 + 教室"，上限避免高窗口里块太空。
const double kMinRowHeight = 44;
const double kMaxRowHeight = 60;

/// 窄窗口展开周末时的最小列宽（再窄就横向滚动，而不是把字挤碎）。
const double kMinDayWidth = 88;

/// 低于该宽度：折叠本周无课的周末列、缩小节次轴。
const double kNarrowWidth = 560;

const List<String> kWeekdayLabels = <String>['一', '二', '三', '四', '五', '六', '日'];

/// 课表网格。
///
/// - [courses]：**本周**要显示的课（调用方已按 `weeks` 过滤）；
/// - [dayCount]：5（折叠周末）或 7；
/// - [dates]：本周 7 天的日期；`null` = 学期开始日未确认，表头不显示日期；
/// - [todayIndex]：1..7，仅"查看的是本周且已确认学期开始日"时给；
/// - [scrollToPeriod]：打开/切周后滚到第几节（通常是今天第一节），`null` = 不滚；
/// - [assignment]：整学期的「课程名 → 色板槽位」映射（见 `course_palette.dart`）。
class TimetableGrid extends StatefulWidget {
  const TimetableGrid({
    super.key,
    required this.courses,
    required this.week,
    required this.dayCount,
    required this.assignment,
    this.dates,
    this.todayIndex,
    this.scrollToPeriod,
  });

  final List<StoredCourse> courses;
  final int week;
  final int dayCount;
  final Map<String, int> assignment;
  final List<DateTime>? dates;
  final int? todayIndex;
  final int? scrollToPeriod;

  @override
  State<TimetableGrid> createState() => _TimetableGridState();
}

class _TimetableGridState extends State<TimetableGrid> {
  final ScrollController _vertical = ScrollController();
  final ScrollController _horizontal = ScrollController();

  /// 最近一次布局算出的行高（自动滚动要用）。
  double _rowHeight = kMinRowHeight;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) => _jumpToTarget());
  }

  @override
  void didUpdateWidget(covariant TimetableGrid oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.week != widget.week ||
        oldWidget.scrollToPeriod != widget.scrollToPeriod ||
        oldWidget.dayCount != widget.dayCount) {
      // 切周后课程全变：回到当前节次位置，别停在上一周的滚动位置。
      WidgetsBinding.instance.addPostFrameCallback((_) => _jumpToTarget());
    }
  }

  @override
  void dispose() {
    _vertical.dispose();
    _horizontal.dispose();
    super.dispose();
  }

  /// 用 `jumpTo`（无动画）：① 不打断用户；② widget test 里 `pumpAndSettle` 不会被动画拖住。
  void _jumpToTarget() {
    final int? period = widget.scrollToPeriod;
    if (!mounted || period == null || !_vertical.hasClients) {
      return;
    }
    final double max = _vertical.position.maxScrollExtent;
    if (max <= 0) {
      return;
    }
    final double target = ((period - 1) * _rowHeight - 8).clamp(0.0, max);
    if (target > 0) {
      _vertical.jumpTo(target);
    }
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final bool dark = theme.brightness == Brightness.dark;

    return LayoutBuilder(
      builder: (BuildContext context, BoxConstraints constraints) {
        final double width = constraints.maxWidth;
        final double height = constraints.maxHeight;
        final bool narrow = width < kNarrowWidth;
        final int days = widget.dayCount.clamp(1, 7);
        final double axisWidth = narrow ? 32 : 36;

        // 行高自适应：让 1280×720 的默认窗口正好放下 10 节（见方案 §2.3.1）。
        final double usable = height.isFinite ? height : 600;
        final double rowHeight = ((usable - kGridHeaderHeight) / kPeriodCount)
            .clamp(kMinRowHeight, kMaxRowHeight);

        // 列宽：宽窗口按可用宽度均分（不滚动）；窄窗口且展开周末时给固定最小宽 + 横向滚动。
        final double fitWidth = math.max(0.0, (width - axisWidth) / days);
        final double dayWidth =
            (narrow && days == 7) ? math.max(fitWidth, kMinDayWidth) : fitWidth;
        final double contentWidth = axisWidth + dayWidth * days;
        final double innerWidth = math.max(contentWidth, width);

        _rowHeight = rowHeight;
        final List<DateTime>? dates = widget.dates;

        return Scrollbar(
          controller: _horizontal,
          thumbVisibility: false,
          child: SingleChildScrollView(
            controller: _horizontal,
            scrollDirection: Axis.horizontal,
            child: SizedBox(
              width: innerWidth,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: <Widget>[
                  _HeaderRow(
                    days: days,
                    axisWidth: axisWidth,
                    dayWidth: dayWidth,
                    dates: dates,
                    todayIndex: widget.todayIndex,
                  ),
                  Expanded(
                    child: SingleChildScrollView(
                      controller: _vertical,
                      child: SizedBox(
                        height: rowHeight * kPeriodCount,
                        child: Stack(
                          children: <Widget>[
                            Positioned.fill(
                              child: CustomPaint(
                                painter: _GridPainter(
                                  rowHeight: rowHeight,
                                  axisWidth: axisWidth,
                                  dayWidth: dayWidth,
                                  dayCount: days,
                                  todayIndex: widget.todayIndex,
                                  lineColor: dark
                                      ? Colors.white.withValues(alpha: 0.10)
                                      : Colors.black.withValues(alpha: 0.06),
                                  todayColor:
                                      theme.colorScheme.primary.withValues(
                                    alpha: dark ? 0.10 : 0.05,
                                  ),
                                ),
                              ),
                            ),
                            Row(
                              crossAxisAlignment: CrossAxisAlignment.stretch,
                              children: <Widget>[
                                SizedBox(
                                  width: axisWidth,
                                  child: _PeriodAxis(rowHeight: rowHeight),
                                ),
                                for (int day = 1; day <= days; day++)
                                  SizedBox(
                                    width: dayWidth,
                                    child: Stack(
                                      clipBehavior: Clip.none,
                                      children: _buildBlocks(
                                          day, dayWidth, rowHeight, theme),
                                    ),
                                  ),
                              ],
                            ),
                          ],
                        ),
                      ),
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

  /// 一天的课程块：同格冲突（不同周次集合导致 Rust 没合并）时横向分道。
  List<Widget> _buildBlocks(
      int day, double dayWidth, double rowHeight, ThemeData theme) {
    final List<StoredCourse> dayCourses = widget.courses
        .where((StoredCourse course) => course.weekday == day)
        .toList(growable: false);
    if (dayCourses.isEmpty) {
      return const <Widget>[];
    }
    final List<_Laned> laned = _lanes(dayCourses);
    final List<Widget> blocks = <Widget>[];
    for (final _Laned item in laned) {
      final StoredCourse course = item.course;
      final int start = course.startPeriod.clamp(1, kPeriodCount);
      final int end = course.endPeriod.clamp(start, kPeriodCount);
      final int span = end - start + 1;
      final double laneWidth = dayWidth / item.laneCount;
      // 几何全用整数节号定位（接口只给节号，没有"半节"），不做时间比例换算。
      // +3 / -6 让块之间留 6px 缝：跨几节一眼可数。
      final double top = (start - 1) * rowHeight + 3;
      final double height = math.max(18.0, span * rowHeight - 6);
      blocks.add(
        Positioned(
          top: top,
          height: height,
          left: item.lane * laneWidth + 2,
          width: math.max(16.0, laneWidth - 4),
          child: _CourseBlock(
            course: course,
            span: span,
            tint: courseTintOf(context, widget.assignment, course.name),
            laneCount: item.laneCount,
            week: widget.week,
            onTap: () => showCourseDetail(
              context,
              course: course,
              week: widget.week,
              assignment: widget.assignment,
            ),
          ),
        ),
      );
    }
    return blocks;
  }

  /// 贪心分道：时间区间不重叠的课共用一道。
  ///
  /// 关键点：**每一道只影响和它真正重叠的课**（`laneCount` 按"与我重叠的课的最老年道 + 1"
  /// 算），所以同一天里 A 处的两门课不会把 B 处的单门课也压窄。
  /// 排序 `(start 升序, end 降序, 课名)`：长块优先占 lane 0，同起点时课名 tie-break，
  /// 保证同样的数据每次渲染分道一致（不会刷新就换位置）。
  List<_Laned> _lanes(List<StoredCourse> dayCourses) {
    final List<StoredCourse> sorted = dayCourses.toList()
      ..sort((StoredCourse a, StoredCourse b) {
        final int byStart = a.startPeriod.compareTo(b.startPeriod);
        if (byStart != 0) {
          return byStart;
        }
        final int byEnd = b.endPeriod.compareTo(a.endPeriod);
        if (byEnd != 0) {
          return byEnd;
        }
        return a.name.compareTo(b.name);
      });
    final List<List<StoredCourse>> lanes = <List<StoredCourse>>[];
    final Map<StoredCourse, int> laneOf = <StoredCourse, int>{};
    for (final StoredCourse course in sorted) {
      int lane = -1;
      for (int i = 0; i < lanes.length; i++) {
        final bool free = lanes[i].every((StoredCourse other) =>
            course.startPeriod > other.endPeriod ||
            course.endPeriod < other.startPeriod);
        if (free) {
          lane = i;
          break;
        }
      }
      if (lane < 0) {
        lanes.add(<StoredCourse>[course]);
        lane = lanes.length - 1;
      } else {
        lanes[lane].add(course);
      }
      laneOf[course] = lane;
    }
    return sorted.map((StoredCourse course) {
      int maxLane = laneOf[course]!;
      for (final StoredCourse other in sorted) {
        final bool overlaps = !(other.endPeriod < course.startPeriod ||
            other.startPeriod > course.endPeriod);
        if (!overlaps) {
          continue;
        }
        maxLane = math.max(maxLane, laneOf[other]!);
      }
      return _Laned(
          course: course, lane: laneOf[course]!, laneCount: maxLane + 1);
    }).toList(growable: false);
  }
}

class _Laned {
  const _Laned(
      {required this.course, required this.lane, required this.laneCount});

  final StoredCourse course;
  final int lane;
  final int laneCount;
}

/// 表头：星期 + 日期；"今天"用主色 pill 填充（不靠颜色深浅区分，灰度也能看出）。
class _HeaderRow extends StatelessWidget {
  const _HeaderRow({
    required this.days,
    required this.axisWidth,
    required this.dayWidth,
    required this.dates,
    required this.todayIndex,
  });

  final int days;
  final double axisWidth;
  final double dayWidth;
  final List<DateTime>? dates;
  final int? todayIndex;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final ColorScheme colors = theme.colorScheme;
    return SizedBox(
      height: kGridHeaderHeight,
      child: Row(
        children: <Widget>[
          SizedBox(width: axisWidth),
          for (int day = 1; day <= days; day++)
            SizedBox(
              width: dayWidth,
              child: _DayHead(
                label: '周${kWeekdayLabels[day - 1]}',
                date: _dateOf(day),
                isToday: todayIndex == day,
                colors: colors,
              ),
            ),
        ],
      ),
    );
  }

  String? _dateOf(int day) {
    final List<DateTime>? value = dates;
    if (value == null || day - 1 >= value.length) {
      return null;
    }
    final DateTime date = value[day - 1];
    final String month = date.month.toString().padLeft(2, '0');
    final String dayText = date.day.toString().padLeft(2, '0');
    return '$month-$dayText';
  }
}

class _DayHead extends StatelessWidget {
  const _DayHead({
    required this.label,
    required this.date,
    required this.isToday,
    required this.colors,
  });

  final String label;
  final String? date;
  final bool isToday;
  final ColorScheme colors;

  @override
  Widget build(BuildContext context) {
    final TextStyle labelStyle = TextStyle(
      fontSize: 12,
      fontWeight: isToday ? FontWeight.w700 : FontWeight.w600,
      color: isToday ? colors.onPrimary : colors.onSurfaceVariant,
    );
    final TextStyle dateStyle = TextStyle(
      fontSize: 10,
      height: 1.1,
      color: isToday
          ? colors.onPrimary.withValues(alpha: 0.85)
          : colors.onSurfaceVariant.withValues(alpha: 0.75),
      fontFeatures: const <FontFeature>[FontFeature.tabularFigures()],
    );
    final Widget content = Column(
      mainAxisAlignment: MainAxisAlignment.center,
      children: <Widget>[
        Text(label,
            style: labelStyle, maxLines: 1, overflow: TextOverflow.ellipsis),
        if (date != null)
          Text(date!,
              style: dateStyle, maxLines: 1, overflow: TextOverflow.ellipsis),
      ],
    );
    if (!isToday) {
      return Padding(
          padding: const EdgeInsets.symmetric(horizontal: 2), child: content);
    }
    return Container(
      margin: const EdgeInsets.symmetric(horizontal: 4, vertical: 5),
      decoration: BoxDecoration(
        color: colors.primary,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Center(
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 2),
          child: FittedBox(fit: BoxFit.scaleDown, child: content),
        ),
      ),
    );
  }
}

/// 左侧节次轴：1..10，数字用等宽数字（对齐），4/8 之后画分组分隔线。
class _PeriodAxis extends StatelessWidget {
  const _PeriodAxis({required this.rowHeight});

  final double rowHeight;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return Column(
      children: <Widget>[
        for (int period = 1; period <= kPeriodCount; period++)
          SizedBox(
            height: rowHeight,
            child: Center(
              child: Text(
                '$period',
                style: theme.textTheme.labelSmall?.copyWith(
                  fontSize: 11,
                  fontWeight: FontWeight.w500,
                  color:
                      theme.colorScheme.onSurfaceVariant.withValues(alpha: 0.8),
                  fontFeatures: const <FontFeature>[
                    FontFeature.tabularFigures()
                  ],
                ),
              ),
            ),
          ),
      ],
    );
  }
}

/// 网格线 + 今天列底色。全部画在底层，课程块浮在上面。
class _GridPainter extends CustomPainter {
  const _GridPainter({
    required this.rowHeight,
    required this.axisWidth,
    required this.dayWidth,
    required this.dayCount,
    required this.todayIndex,
    required this.lineColor,
    required this.todayColor,
  });

  final double rowHeight;
  final double axisWidth;
  final double dayWidth;
  final int dayCount;
  final int? todayIndex;
  final Color lineColor;
  final Color todayColor;

  @override
  void paint(Canvas canvas, Size size) {
    final int? today = todayIndex;
    if (today != null && today >= 1 && today <= dayCount) {
      final double left = axisWidth + (today - 1) * dayWidth;
      canvas.drawRect(
        Rect.fromLTWH(left, 0, dayWidth, size.height),
        Paint()..color = todayColor,
      );
    }

    final Paint thin = Paint()
      ..color = lineColor
      ..strokeWidth = 1;
    for (int row = 1; row < kPeriodCount; row++) {
      final double y = row * rowHeight;
      canvas.drawLine(Offset(0, y), Offset(size.width, y), thin);
    }
    // 上午/下午/晚上 三段的交界画粗一点（不写文字标签，最省视觉噪音）
    final Paint thick = Paint()
      ..color = lineColor
      ..strokeWidth = 2;
    for (final int row in <int>[4, 8]) {
      final double y = row * rowHeight;
      canvas.drawLine(Offset(0, y), Offset(size.width, y), thick);
    }
    for (int day = 0; day <= dayCount; day++) {
      final double x = axisWidth + day * dayWidth;
      canvas.drawLine(Offset(x, 0), Offset(x, size.height), thin);
    }
  }

  @override
  bool shouldRepaint(covariant _GridPainter old) =>
      old.rowHeight != rowHeight ||
      old.dayWidth != dayWidth ||
      old.dayCount != dayCount ||
      old.todayIndex != todayIndex ||
      old.lineColor != lineColor ||
      old.todayColor != todayColor;
}

/// 课程块：课名（最多 2 行）+ 教室 + 「第 x-y 节 · 周次」。
///
/// 三行是**按可用高度逐级降级**的，任何块高都不会溢出：
/// - 高度 ≥ 42 才显示教室；≥ 55（且跨 ≥ 2 节）才显示节次/周次；
/// - 高度够高才允许课名折 2 行。
class _CourseBlock extends StatelessWidget {
  const _CourseBlock({
    required this.course,
    required this.span,
    required this.tint,
    required this.laneCount,
    required this.week,
    required this.onTap,
  });

  final StoredCourse course;
  final int span;
  final CourseTint tint;
  final int laneCount;
  final int week;
  final VoidCallback onTap;

  static const double _padTop = 4;
  static const double _padBottom = 3;
  static const double _padding = _padTop + _padBottom;
  static const double _nameLine = 17;
  static const double _roomLine = 14;
  static const double _metaLine = 13;

  @override
  Widget build(BuildContext context) {
    final String? room =
        (course.room ?? '').trim().isEmpty ? null : course.room!.trim();
    return LayoutBuilder(
      builder: (BuildContext context, BoxConstraints constraints) {
        final double height = constraints.maxHeight;
        final bool showRoom =
            room != null && height >= _padding + _nameLine + _roomLine;
        final bool showMeta = span >= 2 &&
            height >=
                _padding + _nameLine + (showRoom ? _roomLine : 0) + _metaLine;
        final bool twoLineName = laneCount == 1 &&
            height >=
                _padding +
                    2 * _nameLine +
                    (showRoom ? _roomLine : 0) +
                    (showMeta ? _metaLine : 0);

        return Tooltip(
          message: _tooltipText(room),
          waitDuration: const Duration(milliseconds: 600),
          child: Material(
            color: Colors.transparent,
            borderRadius: BorderRadius.circular(8),
            clipBehavior: Clip.antiAlias,
            child: InkWell(
              borderRadius: BorderRadius.circular(8),
              hoverColor: tint.inkSoft.withValues(alpha: 0.10),
              splashColor: tint.inkSoft.withValues(alpha: 0.16),
              onTap: onTap,
              child: Ink(
                decoration: BoxDecoration(
                  color: tint.container,
                  borderRadius: BorderRadius.circular(8),
                  border: Border.all(color: tint.border),
                ),
                child: Stack(
                  children: <Widget>[
                    Positioned(
                      left: 0,
                      top: 6,
                      bottom: 6,
                      width: 3,
                      child: DecoratedBox(
                        decoration: BoxDecoration(
                          color: tint.inkSoft.withValues(alpha: 0.55),
                          borderRadius: BorderRadius.circular(2),
                        ),
                      ),
                    ),
                    Padding(
                      padding:
                          const EdgeInsets.fromLTRB(9, _padTop, 4, _padBottom),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        mainAxisAlignment: MainAxisAlignment.start,
                        children: <Widget>[
                          Text(
                            course.name,
                            maxLines: twoLineName ? 2 : 1,
                            overflow: TextOverflow.ellipsis,
                            style: TextStyle(
                              fontSize: 13,
                              height: 1.3,
                              fontWeight: FontWeight.w600,
                              color: tint.ink,
                            ),
                          ),
                          if (showRoom)
                            Text(
                              room,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(
                                  fontSize: 11,
                                  height: 1.3,
                                  color: tint.inkSoft),
                            ),
                          if (showMeta)
                            Text(
                              _metaText(),
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(
                                  fontSize: 10,
                                  height: 1.3,
                                  color: tint.inkSoft),
                            ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
            ),
          ),
        );
      },
    );
  }

  /// 块被截断时，鼠标悬停能看到完整信息（桌面端的主要补偿手段）。
  String _tooltipText(String? room) {
    final List<String> parts = <String>[
      course.name,
      '周${course.weekdayLabel} ${course.periodLabel}',
      if (room != null) room,
      if ((course.teacher ?? '').trim().isNotEmpty) course.teacher!.trim(),
      if (course.weeks.trim().isNotEmpty) '第 ${course.weeks.trim()} 周',
    ];
    return parts.join(' · ');
  }

  /// `第 1-2 节 · 1-16 周`；只有 1 周时写 `第 1-2 节 · 第 3 周`。
  String _metaText() {
    final String periods = course.periodLabel;
    final String weeks = course.weeks.trim();
    if (weeks.isEmpty) {
      return periods;
    }
    final String weekText =
        weeks.contains('-') || weeks.contains(',') ? '$weeks 周' : '第 $weeks 周';
    return '$periods · $weekText';
  }
}
