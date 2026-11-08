// t121：真尺寸用例——1280×720「整周可见」与 480×900「三主题渲染」。
//
// 尺寸一律用 `setTestWindow`（`setSurfaceSize` 不改 MediaQuery，会造成假绿）。

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/campus/timetable/timetable_grid.dart';
import 'package:yunong/src/core/timetable_service.dart';
import 'package:yunong/src/theme/theme_controller.dart';
import 'package:yunong/src/theme/yunong_tokens.dart';

import 'support/test_window.dart';

/// WCAG 对比度。
double _contrast(Color a, Color b) {
  final double la = a.computeLuminance();
  final double lb = b.computeLuminance();
  final double hi = math.max(la, lb);
  final double lo = math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
}

StoredCourse _course({
  required int id,
  required String name,
  required int weekday,
  required int start,
  required int end,
}) =>
    StoredCourse(
      id: id,
      term: '2026-2027-1',
      name: name,
      weekday: weekday,
      startPeriod: start,
      endPeriod: end,
      weeks: '1-16',
      room: '东${100 + id}',
      teacher: '老师$id',
    );

/// 7 天都有课、含长课名：验证“整周可见”是否真成立（3 门课测不出挤压）。
List<StoredCourse> _fullWeek() => <StoredCourse>[
      _course(id: 1, name: '思想道德与法治（含实践）', weekday: 1, start: 1, end: 2),
      _course(id: 2, name: '高等数学 A（上）', weekday: 1, start: 3, end: 4),
      _course(id: 3, name: '大学英语（读写译）', weekday: 2, start: 1, end: 2),
      _course(id: 4, name: '计算机程序设计基础', weekday: 2, start: 5, end: 6),
      _course(id: 5, name: '线性代数', weekday: 3, start: 3, end: 4),
      _course(id: 6, name: '体育（羽毛球）', weekday: 3, start: 7, end: 8),
      _course(id: 7, name: '植物学实验（含野外）', weekday: 4, start: 1, end: 4),
      _course(id: 8, name: '农业气象学', weekday: 5, start: 3, end: 4),
      _course(id: 9, name: '大学物理实验', weekday: 6, start: 5, end: 8),
      _course(id: 10, name: '形势与政策（专题讲座）', weekday: 7, start: 9, end: 10),
    ];

Widget _gridHost() => MaterialApp(
      home: Scaffold(
        body: TimetableGrid(
          courses: _fullWeek(),
          week: 8,
          dayCount: 7,
          assignment: const <String, int>{},
        ),
      ),
    );

void main() {
  group('1280×720 真尺寸：整周可见', () {
    testWidgets('10 门课全部渲染、内容不超出视口（无需纵向滚动）', (WidgetTester tester) async {
      setTestWindow(tester, 1280, 720);
      await tester.pumpWidget(_gridHost());
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);

      for (final StoredCourse course in _fullWeek()) {
        expect(
          find.textContaining(course.name.substring(0, math.min(4, course.name.length))),
          findsWidgets,
          reason: '缺少课程块：${course.name}',
        );
      }

      for (final ScrollableState state in tester.stateList<ScrollableState>(find.byType(Scrollable))) {
        expect(state.position.maxScrollExtent, 0,
            reason: '1280×720 下不应需要滚动（maxScrollExtent=${state.position.maxScrollExtent}）');
      }
    });

    testWidgets('长课名不被截断到不可读（至少显示前 6 个字）', (WidgetTester tester) async {
      setTestWindow(tester, 1280, 720);
      await tester.pumpWidget(_gridHost());
      await tester.pumpAndSettle();

      const String longName = '思想道德与法治（含实践）';
      final Finder courseText = find.textContaining(longName.substring(0, 6));
      expect(courseText, findsWidgets);

      final Text widget = tester.widget<Text>(courseText.first);
      final String shown = widget.data ?? '';
      expect(shown.length, greaterThanOrEqualTo(6), reason: '实际显示：$shown');
      if (widget.maxLines != null) {
        expect(widget.maxLines, lessThanOrEqualTo(3));
      }
    });
  });

  group('480×900 真尺寸：三主题渲染', () {
    for (final AppThemeId id in AppThemeId.values) {
      testWidgets('${id.label}：无溢出、关键元素在视口内、对比度 ≥ 4.5', (WidgetTester tester) async {
        setTestWindow(tester, 480, 900);
        await tester.pumpWidget(YunongApp(
          themeController: ThemeController(store: MemorySettingsStore(), initialTheme: id),
        ));
        await tester.pumpAndSettle();
        expect(tester.takeException(), isNull, reason: '${id.label} 在 480×900 下不应有溢出/异常');

        // Rect.contains 对右/下边界是**排他**的，rail 正好贴到 900 会被判越界 → 用容差比较
        for (final Finder finder in <Finder>[
          find.byType(TextField).first,
          find.byIcon(Icons.arrow_upward),
          find.byKey(const Key('app-sidebar')),
        ]) {
          expect(finder, findsWidgets);
          final Rect rect = tester.getRect(finder.first);
          expect(
            rect.left >= -0.5 && rect.top >= -0.5 && rect.right <= 480.5 && rect.bottom <= 900.5,
            isTrue,
            reason: '元素超出 480×900 视口：$rect',
          );
        }

        final BuildContext context = tester.element(find.byType(Scaffold).first);
        final YunongTokens tokens = context.tokens;
        final double ratio = _contrast(tokens.ink, tokens.canvas);
        expect(ratio, greaterThanOrEqualTo(4.5), reason: '${id.label} 对比度=$ratio');
      });
    }
  });
}
