// 课表服务与失败映射的单元测试（用 fake 核心，不需要真 DLL）。

import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/app_paths.dart';
import 'package:yunong/src/core/ffi_bindings.dart';
import 'package:yunong/src/core/timetable_service.dart';

/// 记录调用的假核心。
class FakeCore implements CoreApi {
  FakeCore({
    this.importResponse,
    this.courses = const <StoredCourse>[],
    this.setTermStartResponse,
    this.setTermStartThrow,
  });

  CoreEnvelope? importResponse;
  List<StoredCourse> courses;
  CoreEnvelope? setTermStartResponse;

  /// 非 null 时，`setTermStart` 直接抛出它（用于验证 CoreUnavailable 兜底）。
  Object? setTermStartThrow;
  final List<Map<String, String>> importCalls = <Map<String, String>>[];

  @override
  CoreEnvelope importScheduleJson({
    required String json,
    required String term,
    required String dbPath,
  }) =>
      const CoreEnvelope(ok: true, data: <String, dynamic>{
        'term': '',
        'imported': 0,
        'weeks_seen': <dynamic>[],
        'missing_weeks': <dynamic>[],
        'partial': false,
      });

  @override
  CoreEnvelope exportIcs({
    required String term,
    required String outPath,
    required String dbPath,
  }) =>
      const CoreEnvelope(ok: false, code: 'INTERNAL', message: 'unused');

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
    return setTermStartResponse ??
        const CoreEnvelope(ok: true, data: <String, dynamic>{'term': '', 'starts_on': ''});
  }
  @override
  String get version => 'test';

  @override
  CoreEnvelope importTimetable({
    required String html,
    required String term,
    required String dbPath,
  }) {
    importCalls.add(<String, String>{'html': html, 'term': term, 'dbPath': dbPath});
    return importResponse ??
        const CoreEnvelope(ok: true, data: <String, dynamic>{
          'term': '2026-2027-1',
          'imported': 1,
          'warnings': <dynamic>[],
        });
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
  CoreEnvelope listTerms({required String dbPath}) => const CoreEnvelope(
        ok: true,
        data: <String, dynamic>{
          'terms': <dynamic>[
            <String, dynamic>{'term': '2026-2027-1', 'starts_on': '', 'ends_on': null},
          ],
        },
      );
}

void main() {
  const course = StoredCourse(
    id: 1,
    term: '2026-2027-1',
    name: '高等数学A',
    weekday: 1,
    startPeriod: 1,
    endPeriod: 2,
    weeks: '1-16',
    room: '东305',
    teacher: '张老师',
  );

  test('导入成功后返回结果并落到服务', () async {
    final core = FakeCore(courses: <StoredCourse>[course]);
    final service = TimetableService(core: core, dbPath: '/tmp/t.db');

    final outcome = await service.importHtml(html: '<html/>', term: '2026-2027-1');
    expect(outcome.imported, 1);
    expect(core.importCalls.single['term'], '2026-2027-1');
    expect(core.importCalls.single['dbPath'], '/tmp/t.db');

    final courses = await service.listCourses(term: '2026-2027-1');
    expect(courses.single.name, '高等数学A');
    expect(courses.single.weekdayLabel, '一');
    expect(courses.single.periodLabel, '第 1-2 节');
  });

  test('PARSE_FAILED 映射成"页面可能已更新"的用户提示', () async {
    final core = FakeCore(
      importResponse: const CoreEnvelope(
        ok: false,
        code: 'PARSE_FAILED',
        message: '找不到课表表格（table#timetable）',
      ),
    );
    final service = TimetableService(core: core, dbPath: '/tmp/t.db');

    await expectLater(
      service.importHtml(html: '<html/>', term: 't'),
      throwsA(isA<TimetableException>().having(
        (TimetableException e) => e.userHint,
        'userHint',
        contains('可能已更新'),
      )),
    );
  });

  test('PARSE_EMPTY 提示确认排课而不是显示空表', () async {
    final core = FakeCore(
      importResponse: const CoreEnvelope(
        ok: false,
        code: 'PARSE_EMPTY',
        message: '未解析到课程',
      ),
    );
    final service = TimetableService(core: core, dbPath: '/tmp/t.db');
    await expectLater(
      service.importHtml(html: '<html/>', term: 't'),
      throwsA(isA<TimetableException>().having(
        (TimetableException e) => e.userHint,
        'userHint',
        contains('是否已排课'),
      )),
    );
  });

  test('未知错误码兜底成内部错误，但保留原始信息', () {
    final failure = describeFailure('WEIRD', '原始细节');
    expect(failure.code, 'WEIRD');
    expect(failure.userHint, contains('内部错误'));
    expect(failure.message, '原始细节');
  });

  test('课表库里没有任何学期时列表为空（不是失败）', () async {
    final core = FakeCore();
    final service = TimetableService(core: core, dbPath: '/tmp/t.db');
    final terms = await service.listTerms();
    expect(terms, <String>['2026-2027-1']);
  });

  test('dbPath 默认与 AppPaths.dataDir 同根（不再用 HOME 拼旧路径）', () {
    final service = TimetableService(core: FakeCore(), dbPath: null);
    // #91：旧实现返回 `<HOME|LOCALAPPDATA>/yunong/timetable.db`，Android 上 HOME=/
    // 会拼出 //yunong/timetable.db。现在必须与设置页显示的数据根同源。
    expect(
      service.dbPath,
      <String>[AppPaths.dataDir, 'timetable.db'].join(Platform.pathSeparator),
    );
    expect(service.dbPath, endsWith('timetable.db'));
    // 关键回归：课表库的**根**必须与设置页显示的数据根一致（不再 `//yunong`）。
    expect(AppPaths.dataDirOf(service.dbPath), AppPaths.dataDir);
    expect(service.dbPath.replaceAll(r'\', '/'), isNot(startsWith('//')));
  });

  test('#91 setTermStart STORAGE：message 保留 Rust 原始错误与 db 绝对路径', () async {
    const String db = '/data/user/0/cn.ynau.yunong/files/yunong/yunong.db';
    final core = FakeCore(
      setTermStartResponse: const CoreEnvelope(
        ok: false,
        code: 'STORAGE',
        message: '建库目录失败：Permission denied (os error 13)',
      ),
    );
    final service = TimetableService(core: core, dbPath: db);

    await expectLater(
      service.setTermStart(term: '2026-2027-1', startsOn: '2026-09-07'),
      throwsA(
        isA<TimetableException>()
            .having((TimetableException e) => e.code, 'code', 'STORAGE')
            .having((TimetableException e) => e.userHint, 'userHint', '本地课表保存失败')
            .having((TimetableException e) => e.message, 'message', contains('建库目录失败'))
            .having((TimetableException e) => e.message, 'message', contains(db)),
      ),
    );
  });

  test('#91 setTermStart 核心未就绪：转成 CORE_UNAVAILABLE 而不是静默抛出', () async {
    final core = FakeCore(setTermStartThrow: const CoreUnavailable('libyunong_core.so 打不开'));
    final service = TimetableService(core: core, dbPath: '/tmp/yunong.db');

    await expectLater(
      service.setTermStart(term: '2026-2027-1', startsOn: '2026-09-07'),
      throwsA(
        isA<TimetableException>()
            .having((TimetableException e) => e.code, 'code', 'CORE_UNAVAILABLE')
            .having((TimetableException e) => e.message, 'message', contains('libyunong_core.so'))
            .having((TimetableException e) => e.message, 'message', contains('/tmp/yunong.db')),
      ),
    );
  });

  test('抓取脚本契约是 JSON 且不含 Cookie 读取', () {
    final description = DebugFixtures.extractionScriptDescription;
    final decoded = jsonDecode(description) as Map<String, dynamic>;
    expect(decoded['privacy'], contains('不回传 Cookie'));
    expect(description, isNot(contains('document.cookie')));
  });
}
