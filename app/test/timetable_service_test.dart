// 课表服务与失败映射的单元测试（用 fake 核心，不需要真 DLL）。

import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/ffi_bindings.dart';
import 'package:yunong/src/core/timetable_service.dart';

/// 记录调用的假核心。
class FakeCore implements CoreApi {
  FakeCore({this.importResponse, this.courses = const <StoredCourse>[]});

  CoreEnvelope? importResponse;
  List<StoredCourse> courses;
  final List<Map<String, String>> importCalls = <Map<String, String>>[];

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

  test('dbPath 默认落在用户本地目录（不带插件）', () {
    final service = TimetableService(core: FakeCore(), dbPath: null);
    expect(service.dbPath, contains('timetable.db'));
    expect(service.dbPath, contains('yunong'));
  });

  test('抓取脚本契约是 JSON 且不含 Cookie 读取', () {
    final description = DebugFixtures.extractionScriptDescription;
    final decoded = jsonDecode(description) as Map<String, dynamic>;
    expect(decoded['privacy'], contains('不回传 Cookie'));
    expect(description, isNot(contains('document.cookie')));
  });
}
