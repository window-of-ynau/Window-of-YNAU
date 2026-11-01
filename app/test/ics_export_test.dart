// W7-2：ICS 导出的服务层 + UI 入口测试（fake core，离线）。
//
// 覆盖：
// - 成功：路径与事件数正确展示；
// - TERM_START_UNKNOWN：可读文案 + “去确认学期开始日”入口；
// - 写入失败（STORAGE）：通用提示，**不显示原始错误串**；
// - 不静默覆盖：同名文件自动加 (2)。

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/timetable_page.dart';
import 'package:yunong/src/core/ffi_bindings.dart';
import 'package:yunong/src/core/timetable_service.dart';

/// 只实现本测试用到的方法的假核心。
class FakeCore implements CoreApi {
  FakeCore({this.exportResponse});

  CoreEnvelope? exportResponse;
  final List<String> exportedPaths = <String>[];

  @override
  String get version => 'test';

  @override
  CoreEnvelope exportIcs({
    required String term,
    required String outPath,
    required String dbPath,
  }) {
    exportedPaths.add(outPath);
    return exportResponse ??
        CoreEnvelope(ok: true, data: <String, dynamic>{
          'path': outPath,
          'events': 42,
        });
  }

  @override
  CoreEnvelope importTimetable({
    required String html,
    required String term,
    required String dbPath,
  }) =>
      const CoreEnvelope(ok: false, code: 'INTERNAL', message: 'unused');

  @override
  CoreEnvelope importScheduleJson({
    required String json,
    required String term,
    required String dbPath,
  }) =>
      const CoreEnvelope(ok: false, code: 'INTERNAL', message: 'unused');

  @override
  CoreEnvelope listCourses({required String term, required String dbPath}) =>
      const CoreEnvelope(ok: true, data: <String, dynamic>{'courses': <dynamic>[]});

  @override
  CoreEnvelope listTerms({required String dbPath}) =>
      const CoreEnvelope(ok: true, data: <String, dynamic>{'terms': <dynamic>[]});

  @override
  CoreEnvelope setTermStart({
    required String term,
    required String startsOn,
    required String dbPath,
  }) =>
      const CoreEnvelope(ok: true, data: <String, dynamic>{});
}

TimetableService serviceWith(FakeCore core, {bool Function(String)? pathExists}) =>
    TimetableService(
      core: core,
      dbPath: '/tmp/timetable.db',
      exportDir: '/tmp/exports',
      pathExists: pathExists ?? ((String _) => false),
    );

void main() {
  group('导出文件名与不覆盖', () {
    test('文件名为 云农之窗-课表-<term>.ics', () {
      expect(icsFileName('2026-2027-1'), '云农之窗-课表-2026-2027-1.ics');
    });

    test('目标已存在时自动加 (2)/(3)，不静默覆盖', () {
      final sep = Platform.pathSeparator;
      final dir = <String>['', 'tmp', 'exports'].join(sep);
      final existing = <String>{'$dir${sep}云农之窗-课表-t.ics', '$dir${sep}云农之窗-课表-t (2).ics'};
      final path = uniqueIcsPath(
        directory: dir,
        fileName: '云农之窗-课表-t.ics',
        exists: existing.contains,
      );
      expect(path, endsWith('云农之窗-课表-t (3).ics'));
    });

    test('目录里同类文件过多时给可读错误（不无限重试）', () {
      expect(
        () => uniqueIcsPath(
          directory: '/tmp/exports',
          fileName: 'x.ics',
          exists: (String _) => true,
          maxAttempts: 3,
        ),
        throwsA(isA<TimetableException>().having(
          (TimetableException e) => e.code,
          'code',
          'EXPORT_PATH_BUSY',
        )),
      );
    });
  });

  group('服务层导出', () {
    test('成功：路径与事件数来自后端', () async {
      final core = FakeCore();
      final service = serviceWith(core);
      final result = await service.exportIcs(term: '2026-2027-1');
      expect(result.events, 42);
      expect(result.path, endsWith('云农之窗-课表-2026-2027-1.ics'));
      expect(core.exportedPaths.single, result.path);
    });

    test('TERM_START_UNKNOWN：中文可读文案，不暴露原始串', () async {
      final core = FakeCore(
        exportResponse: const CoreEnvelope(
          ok: false,
          code: 'TERM_START_UNKNOWN',
          message: 'term starts_on is empty for term 2026-2027-1',
        ),
      );
      final service = serviceWith(core);
      await expectLater(
        service.exportIcs(term: '2026-2027-1'),
        throwsA(isA<TimetableException>()
            .having((TimetableException e) => e.code, 'code', 'TERM_START_UNKNOWN')
            .having((TimetableException e) => e.userHint, 'userHint', contains('第 1 周日期还没确认'))),
      );
    });

    test('STORAGE：通用失败提示', () async {
      final core = FakeCore(
        exportResponse: const CoreEnvelope(
          ok: false,
          code: 'STORAGE',
          message: 'io error: permission denied (os error 5)',
        ),
      );
      final service = serviceWith(core);
      await expectLater(
        service.exportIcs(term: '2026-2027-1'),
        throwsA(isA<TimetableException>()
            .having((TimetableException e) => e.userHint, 'userHint', isNot(contains('os error')))),
      );
    });
  });

  group('UI：导出按钮与结果展示', () {
    testWidgets('成功后显示路径与事件数', (WidgetTester tester) async {
      final core = FakeCore();
      final service = serviceWith(core);
      await tester.pumpWidget(MaterialApp(home: Scaffold(body: TimetablePage(service: service))));

      await tester.tap(find.text('导出到日历'));
      await tester.pumpAndSettle();

      // 顶栏 chip 与结果卡片都会写事件数，所以这里不限定个数，只验证内容可见\n      expect(find.textContaining('已导出 42 个事件'), findsWidgets);
      expect(find.textContaining('云农之窗-课表-2026-2027-1.ics'), findsOneWidget);
      expect(find.textContaining('事件到日历文件'), findsOneWidget);
    });

    testWidgets('TERM_START_UNKNOWN：显示可读提示与“去确认”入口', (WidgetTester tester) async {
      final core = FakeCore(
        exportResponse: const CoreEnvelope(
          ok: false,
          code: 'TERM_START_UNKNOWN',
          message: 'term starts_on is empty',
        ),
      );
      final service = serviceWith(core);
      await tester.pumpWidget(MaterialApp(home: Scaffold(body: TimetablePage(service: service))));

      await tester.tap(find.text('导出到日历'));
      await tester.pumpAndSettle();

      expect(find.textContaining('第 1 周日期还没确认'), findsOneWidget);
      expect(find.text('去确认学期开始日'), findsOneWidget);
      // 不显示原始错误串
      expect(find.textContaining('starts_on is empty'), findsNothing);
    });
  });
}
