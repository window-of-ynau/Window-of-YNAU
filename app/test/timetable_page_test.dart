// 课表页三态与失败文案的 widget 测试（fake 核心，不需要 DLL / WebView）。

import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/timetable_page.dart';
import 'package:yunong/src/core/ffi_bindings.dart';
import 'package:yunong/src/core/timetable_service.dart';

/// 按队列返回响应的假核心。
class ScriptedCore implements CoreApi {
  ScriptedCore(this.importResponses);

  final List<CoreEnvelope> importResponses;
  List<StoredCourse> courses = const <StoredCourse>[];

  @override
  String get version => 'test';

  @override
  CoreEnvelope importTimetable({
    required String html,
    required String term,
    required String dbPath,
  }) {
    if (importResponses.isEmpty) {
      return const CoreEnvelope(ok: false, code: 'INTERNAL', message: '没有脚本了');
    }
    return importResponses.removeAt(0);
  }

  @override
  CoreEnvelope listCourses({required String term, required String dbPath}) =>
      CoreEnvelope(ok: true, data: <String, dynamic>{
        'term': term,
        'courses': courses
            .map((StoredCourse course) => <String, dynamic>{
                  'id': course.id,
                  'term': course.term,
                  'name': course.name,
                  'weekday': course.weekday,
                  'start_period': course.startPeriod,
                  'end_period': course.endPeriod,
                  'weeks': course.weeks,
                  'room': course.room,
                  'teacher': course.teacher,
                })
            .toList(),
      });

  @override
  CoreEnvelope listTerms({required String dbPath}) =>
      const CoreEnvelope(ok: true, data: <String, dynamic>{'terms': <dynamic>[]});
}

Widget _wrap(Widget child) => MaterialApp(home: Scaffold(body: child));

void main() {
  testWidgets('导入成功后显示课程列表', (WidgetTester tester) async {
    final core = ScriptedCore(<CoreEnvelope>[
      const CoreEnvelope(ok: true, data: <String, dynamic>{
        'term': '2026-2027-1',
        'imported': 1,
        'warnings': <dynamic>[],
      }),
    ])..courses = const <StoredCourse>[
        StoredCourse(
          id: 1,
          term: '2026-2027-1',
          name: '高等数学A',
          weekday: 1,
          startPeriod: 1,
          endPeriod: 2,
          weeks: '1-16',
          room: '东305',
          teacher: '张老师',
        ),
      ];
    final service = TimetableService(core: core, dbPath: '/tmp/t.db');

    await tester.pumpWidget(_wrap(TimetablePage(service: service)));
    expect(find.text('还没有导入课表'), findsOneWidget);

    await tester.tap(find.text('导入内置样本'));
    await tester.pumpAndSettle();

    expect(find.text('高等数学A'), findsOneWidget);
    expect(find.textContaining('第 1-2 节'), findsOneWidget);
    expect(find.textContaining('本学期 1 门课'), findsOneWidget);
  });

  testWidgets('页面改版时显示可读提示，且不显示空课表', (WidgetTester tester) async {
    final core = ScriptedCore(<CoreEnvelope>[
      const CoreEnvelope(
        ok: false,
        code: 'PARSE_FAILED',
        message: '找不到课表表格（table#timetable）',
      ),
    ]);
    final service = TimetableService(core: core, dbPath: '/tmp/t.db');

    await tester.pumpWidget(_wrap(TimetablePage(service: service)));
    await tester.tap(find.text('模拟页面改版'));
    await tester.pumpAndSettle();

    // 面向用户的提示
    expect(find.text('学校页面可能已更新，请反馈'), findsOneWidget);
    // 明确不是"没课"
    expect(find.textContaining('这不是"没有课"'), findsOneWidget);
    // 错误码可见，便于用户反馈
    expect(find.textContaining('PARSE_FAILED'), findsOneWidget);
    // 绝不显示空态文案
    expect(find.text('还没有导入课表'), findsNothing);
  });

  testWidgets('没有 WebView 时点"登录并抓取"给出解释而不是静默失败', (WidgetTester tester) async {
    final core = ScriptedCore(<CoreEnvelope>[]);
    final service = TimetableService(core: core, dbPath: '/tmp/t.db');

    await tester.pumpWidget(_wrap(TimetablePage(service: service)));
    await tester.tap(find.text('登录并抓取课表'));
    await tester.pumpAndSettle();

    expect(find.text('暂时无法在应用内登录校务系统'), findsOneWidget);
    expect(find.textContaining('NO_WEBVIEW'), findsOneWidget);
  });

  testWidgets('导入失败后不清空已有列表（失败不等于课表消失）', (WidgetTester tester) async {
    final core = ScriptedCore(<CoreEnvelope>[
      const CoreEnvelope(ok: true, data: <String, dynamic>{
        'term': '2026-2027-1',
        'imported': 1,
        'warnings': <dynamic>[],
      }),
      const CoreEnvelope(ok: false, code: 'PARSE_EMPTY', message: '未解析到课程'),
    ])..courses = const <StoredCourse>[
        StoredCourse(
          id: 1,
          term: '2026-2027-1',
          name: '高等数学A',
          weekday: 1,
          startPeriod: 1,
          endPeriod: 2,
          weeks: '1-16',
        ),
      ];
    final service = TimetableService(core: core, dbPath: '/tmp/t.db');

    await tester.pumpWidget(_wrap(TimetablePage(service: service)));
    await tester.tap(find.text('导入内置样本'));
    await tester.pumpAndSettle();
    expect(find.text('高等数学A'), findsOneWidget);

    await tester.tap(find.text('模拟页面改版'));
    await tester.pumpAndSettle();

    expect(find.textContaining('请确认本学期是否已排课'), findsOneWidget);
    expect(find.text('高等数学A'), findsOneWidget, reason: '旧列表要保留');
  });

  testWidgets('外壳契约：调试入口的说明里点明 Cookie 不上传', (WidgetTester tester) async {
    final core = ScriptedCore(<CoreEnvelope>[]);
    final service = TimetableService(core: core, dbPath: '/tmp/t.db');
    await tester.pumpWidget(_wrap(TimetablePage(service: service)));
    expect(find.textContaining('不上传、不打印'), findsOneWidget);
    // 取数说明是给人看的，不能是空 JSON 之类的占位
    expect(jsonDecode(DebugFixtures.extractionScriptDescription), isA<Map<String, dynamic>>());
  });
}
