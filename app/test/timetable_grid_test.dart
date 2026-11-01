// 网格渲染与稳定着色的回归测试（新增文件，不改动既有测试）。
//
// 覆盖 owner 拍板的几条硬要求：
// - ≤9 门课两两不同色，且跨调用稳定（同一份课表算两次结果一致）；
// - 7 天 × 10 节的真实网格，课程块按节次跨度撑高；
// - 800×600 与 480×900 都不溢出（溢出会在 debug 里抛异常，`takeException` 能抓到）；
// - 点课程块弹详情面板（教师/教室/周次都在里面）。
//
// 注意：网格**不猜作息时间**，所以这里也不断言"当前节次"之类需要作息表的行为。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/timetable/course_palette.dart';
import 'package:yunong/src/campus/timetable/timetable_grid.dart';
import 'package:yunong/src/core/timetable_service.dart';

StoredCourse _course({
  String name = '高等数学A',
  int weekday = 1,
  int start = 1,
  int end = 2,
  String weeks = '1-16',
  String? room = '东305',
  String? teacher = '张老师',
  int id = 1,
}) {
  return StoredCourse(
    id: id,
    term: '2026-2027-1',
    name: name,
    weekday: weekday,
    startPeriod: start,
    endPeriod: end,
    weeks: weeks,
    room: room,
    teacher: teacher,
  );
}

Widget _host(Widget child, {double width = 800, double height = 600}) {
  return MaterialApp(
    home: Scaffold(
      body: Center(
        child: SizedBox(width: width, height: height, child: child),
      ),
    ),
  );
}

void main() {
  group('课程稳定着色（course_palette）', () {
    test('≤9 门课两两不同色', () {
      const List<String> names = <String>[
        '高等数学I',
        '大学英语',
        '数据结构',
        '操作系统',
        '体育',
        '大学物理',
        '马克思主义基本原理',
        '计算机网络',
        '形势与政策',
      ];
      final Map<String, int> assigned = assignCourseTints(names);
      expect(assigned.length, names.length);
      expect(assigned.values.toSet().length, names.length, reason: '12 门课应有 12 个不同槽位');
    });

    test('同一输入算两次结果一致（跨重启/切周稳定）', () {
      final List<String> names = <String>['高等数学I', '大学英语', '数据结构'];
      expect(assignCourseTints(names), assignCourseTints(names));
    });

    test('课程集合变大时，排序最前的课颜色不变（后面可能被挤动，已知取舍）', () {
      final Map<String, int> few = assignCourseTints(<String>['大学英语', '数据结构']);
      final Map<String, int> many =
          assignCourseTints(<String>['大学英语', '数据结构', '操作系统', '高等数学I']);
      // 排序最前的课没有任何"前人"占用槽位，任何课程集合下都是同一个槽位
      expect(many['大学英语'], few['大学英语']);
    });

    test('超过 22 个槽位也不抛异常（允许复用）', () {
      final List<String> names =
          List<String>.generate(24, (int index) => '课程$index');
      final Map<String, int> assigned = assignCourseTints(names);
      expect(assigned.length, 24);
      expect(assigned.values.every((int index) => index >= 0 && index < 22), isTrue);
    });

    test('指纹不是 Dart 的 String.hashCode（跨版本保持稳定）', () {
      // 手算值锁定：换了哈希实现这条会红，提醒别破坏"同课同色"。
      expect(courseNameFingerprint('高等数学I'), 1244771779);
      expect(courseNameFingerprint('大学英语'), 2225081962);
    });
  });

  group('课表网格', () {
    testWidgets('7 天 × 10 节的网格：表头 7 列、节次轴 1..10', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 600));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      await tester.pumpWidget(_host(
        TimetableGrid(
          courses: <StoredCourse>[_course()],
          week: 8,
          dayCount: 7,
          assignment: assignCourseTints(<String>['高等数学A']),
        ),
      ));

      for (final String label in kWeekdayLabels) {
        expect(find.text('周$label'), findsOneWidget, reason: '缺少表头 周$label');
      }
      for (int period = 1; period <= kPeriodCount; period++) {
        expect(find.text('$period'), findsOneWidget, reason: '缺少第 $period 节');
      }
      expect(tester.takeException(), isNull);
    });

    testWidgets('800×600 不溢出，且块里能看到课名 + 教室 + 节次', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 600));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      await tester.pumpWidget(_host(
        TimetableGrid(
          courses: <StoredCourse>[
            _course(),
            _course(name: '大学英语', weekday: 2, start: 3, end: 4, id: 2),
          ],
          week: 8,
          dayCount: 7,
          assignment: assignCourseTints(<String>['高等数学A', '大学英语']),
        ),
      ));

      expect(tester.takeException(), isNull, reason: '800×600 下不允许有 RenderFlex 溢出');
      expect(find.text('高等数学A'), findsOneWidget);
      expect(find.text('东305'), findsWidgets);
      expect(find.textContaining('第 1-2 节'), findsOneWidget);
    });

    testWidgets('480×900 窄窗（折叠周末 = 5 列）不溢出', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(480, 900));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      await tester.pumpWidget(_host(
        TimetableGrid(
          courses: <StoredCourse>[_course()],
          week: 8,
          dayCount: 5,
          assignment: assignCourseTints(<String>['高等数学A']),
        ),
        width: 480,
        height: 900,
      ));

      expect(tester.takeException(), isNull, reason: '窄窗下不允许溢出');
      expect(find.text('周五'), findsOneWidget);
      expect(find.text('周六'), findsNothing, reason: '折叠后不应出现周末列');
    });

    testWidgets('课程块按节次跨度撑高：2 节的块约是单节块的 2 倍', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 600));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      await tester.pumpWidget(_host(
        TimetableGrid(
          courses: <StoredCourse>[
            _course(name: '两节课', weekday: 1, start: 1, end: 2, id: 1),
            _course(name: '一节课', weekday: 3, start: 5, end: 5, id: 2),
          ],
          week: 8,
          dayCount: 7,
          assignment: assignCourseTints(<String>['两节课', '一节课']),
        ),
      ));

      final Size twoPeriod =
          tester.getSize(find.ancestor(of: find.text('两节课'), matching: find.byType(InkWell)).first);
      final Size onePeriod =
          tester.getSize(find.ancestor(of: find.text('一节课'), matching: find.byType(InkWell)).first);
      expect(twoPeriod.height, greaterThan(onePeriod.height * 1.7),
          reason: '跨 2 节的块应明显高于单节块（${twoPeriod.height} vs ${onePeriod.height}）');
    });

    testWidgets('同格冲突横向分道：两门课并排，互不覆盖', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 600));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      await tester.pumpWidget(_host(
        TimetableGrid(
          courses: <StoredCourse>[
            _course(name: '体育', weekday: 4, start: 7, end: 8, id: 1),
            _course(name: '计算机网络', weekday: 4, start: 7, end: 8, id: 2),
          ],
          week: 8,
          dayCount: 7,
          assignment: assignCourseTints(<String>['体育', '计算机网络']),
        ),
      ));

      expect(tester.takeException(), isNull);
      final Rect sports =
          tester.getRect(find.ancestor(of: find.text('体育'), matching: find.byType(InkWell)).first);
      final Rect network = tester.getRect(
          find.ancestor(of: find.text('计算机网络'), matching: find.byType(InkWell)).first);
      expect(sports.overlaps(network), isFalse, reason: '冲突课应分道并排，不能重叠');
      expect((sports.center.dx - network.center.dx).abs(), greaterThan(20),
          reason: '两道之间应有可见横向偏移');
    });

    testWidgets('点课程块弹详情：教师/教室/周次都在', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 600));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      await tester.pumpWidget(_host(
        TimetableGrid(
          courses: <StoredCourse>[_course()],
          week: 8,
          dayCount: 7,
          assignment: assignCourseTints(<String>['高等数学A']),
        ),
      ));

      await tester.tap(find.text('高等数学A'));
      await tester.pumpAndSettle();

      expect(find.text('张老师'), findsOneWidget);
      expect(find.text('东305'), findsWidgets);
      expect(find.textContaining('第 1-16 周'), findsOneWidget);
    });

    testWidgets('没有学期开始日 → 表头不显示日期，也不高亮今天', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 600));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      await tester.pumpWidget(_host(
        TimetableGrid(
          courses: <StoredCourse>[_course()],
          week: 8,
          dayCount: 7,
          assignment: assignCourseTints(<String>['高等数学A']),
          // dates / todayIndex 都不给 = 未确认学期开始日
        ),
      ));

      expect(find.textContaining(RegExp(r'^\d{2}-\d{2}$')), findsNothing);
      expect(tester.takeException(), isNull);
    });

    testWidgets('有日期时显示 MM-DD 表头', (WidgetTester tester) async {
      await tester.binding.setSurfaceSize(const Size(800, 600));
      addTearDown(() => tester.binding.setSurfaceSize(null));

      final DateTime monday = DateTime(2026, 10, 26);
      await tester.pumpWidget(_host(
        TimetableGrid(
          courses: <StoredCourse>[_course()],
          week: 8,
          dayCount: 7,
          assignment: assignCourseTints(<String>['高等数学A']),
          dates: List<DateTime>.generate(7, (int index) => monday.add(Duration(days: index))),
          todayIndex: 3,
        ),
      ));

      expect(find.text('10-26'), findsOneWidget);
      expect(find.text('11-01'), findsOneWidget);
      expect(tester.takeException(), isNull);
    });
  });
}
