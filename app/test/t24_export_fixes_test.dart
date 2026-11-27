// t12 评审 F2/F3/F9 交叉验证：
// - F2：ICS 导出目录走 AppPaths.dataDir（Android 不再 //yunong/exports）；
// - F3：INTERNAL 不再整体放开；setTermStart 用专用码，导出失败只给可读文案；
// - F9：探测 JSON 解析失败不进 silentErrors。

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/campus/timetable_page.dart';
import 'package:yunong/src/core/app_paths.dart';
import 'package:yunong/src/core/ffi_bindings.dart';
import 'package:yunong/src/core/timetable_service.dart';

class _FakeCore implements CoreApi {
  _FakeCore({this.exportResponse, this.setTermStartThrow});

  CoreEnvelope? exportResponse;
  Object? setTermStartThrow;

  @override
  String get version => 'test';

  @override
  CoreEnvelope exportIcs({
    required String term,
    required String outPath,
    required String dbPath,
  }) =>
      exportResponse ??
      CoreEnvelope(ok: true, data: <String, dynamic>{
        'path': outPath,
        'events': 1,
      });

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
  }) {
    final Object? thrown = setTermStartThrow;
    if (thrown != null) {
      throw thrown;
    }
    return const CoreEnvelope(ok: true, data: <String, dynamic>{});
  }
}

TimetableService _service(_FakeCore core) => TimetableService(
      core: core,
      dbPath: '/tmp/timetable.db',
      exportDir: '/tmp/exports',
      pathExists: (String _) => false,
    );

void main() {
  test('F2：Android 导出路径 = 应用私有数据根/exports，不以 // 开头', () {
    const String androidBase = '/data/user/0/cn.ynau.yunong/files';
    final String dataDir = AppPaths.dataDirUnder(androidBase, debug: false);
    final String exports = TimetableService.exportDirectoryUnder(dataDir);

    expect(dataDir.replaceAll('\\', '/'), '/data/user/0/cn.ynau.yunong/files/yunong');
    expect(exports.startsWith('//'), isFalse, reason: '旧实现会产出 //yunong/exports');
    expect(
      exports.replaceAll('\\', '/'),
      '/data/user/0/cn.ynau.yunong/files/yunong/exports',
      reason: '导出目录必须与 dbPath 同根',
    );
  });

  test('F2：默认 exportDirectory 跟随 AppPaths.dataDir（无 override）', () {
    final Directory temp = Directory.systemTemp.createTempSync('yunong_t24_');
    try {
      AppPaths.initDataRoot(
        env: const <String, String>{},
        args: <String>['--data-dir=${temp.path}'],
      );
      final String expected =
          <String>[AppPaths.dataDir, 'exports'].join(Platform.pathSeparator);
      expect(TimetableService(core: _FakeCore()).exportDirectory, expected);
    } finally {
      AppPaths.initDataRoot(env: const <String, String>{}, args: const <String>[]);
      if (temp.existsSync()) {
        temp.deleteSync(recursive: true);
      }
    }
  });

  test('F3：setTermStart 通用异常用专用错误码（不再借 INTERNAL 放开原文）', () async {
    final _FakeCore core = _FakeCore(setTermStartThrow: StateError('sqlite locked'));
    await expectLater(
      _service(core).setTermStart(term: '2026-2027-1', startsOn: '2026-09-07'),
      throwsA(isA<TimetableException>()
          .having((TimetableException e) => e.code, 'code', 'TERM_START_WRITE_FAILED')
          .having((TimetableException e) => e.userHint, 'userHint', contains('本地课表保存失败'))
          .having((TimetableException e) => e.message, 'message', contains('sqlite locked'))),
    );
  });

  testWidgets('F3：导出 STORAGE 失败只给可读文案，不显示原始错误串', (WidgetTester tester) async {
    final _FakeCore core = _FakeCore(
      exportResponse: const CoreEnvelope(
        ok: false,
        code: 'STORAGE',
        message: 'io error: permission denied (os error 5)',
      ),
    );
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(body: TimetablePage(service: _service(core))),
    ));
    await tester.tap(find.text('导出到日历'));
    await tester.pumpAndSettle();

    expect(find.textContaining('本地课表保存失败'), findsWidgets);
    expect(find.textContaining('os error 5'), findsNothing,
        reason: '导出失败不得把原始异常串显示给用户');
  });

  test('F9：探测 JSON 失败单独归类 badjson，不进 silentErrors', () {
    expect(kPortalScheduleScript.contains("noteError('probe.json'"), isFalse);
    expect(kPortalScheduleScript.contains("':0:badjson'"), isTrue);
    expect(kPortalScheduleScript.contains('probeBadJson'), isTrue);
  });
}
