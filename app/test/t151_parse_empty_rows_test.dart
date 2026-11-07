// t151：PARSE_EMPTY 二次修复 —— 就绪判定加"课程行数 > 0"、解析 0 门也重试、诊断带 cardId。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/capture_cache.dart';
import 'package:yunong/src/campus/capture_readiness.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/core/timetable_service.dart';

const String _tableWithRows =
    '<html><title>我的课表</title><table id="timetable">'
    '<tr><th>节次</th></tr><tr><td>高等数学</td></tr><tr><td>大学英语</td></tr></table></html>';

void main() {
  group('就绪判定要求课程行 > 0（t151）', () {
    test('选择器与抓取/解析用的一致：table#timetable tr', () {
      expect(kCourseRowSelector, 'table#timetable tr');
      expect(kGrabHtmlInstallScript.contains("querySelector('table#timetable')"), isTrue);
      expect(kCourseRowCountInstallScript.contains("querySelectorAll('table#timetable tr')"), isTrue);
    });

    test('行数 0 → 未就绪（即使长度够、含课表关键字）', () {
      const CaptureSnapshot snapshot = CaptureSnapshot(
        html: '<html><title>我的课表</title><div>课表 课程 节次 周次</div>${'x'}</html>',
        title: '我的课表',
        tries: 1,
        atMs: 0,
        rowCount: 0,
      );
      expect(snapshot.hasTimetableMark, isTrue);
      expect(snapshot.isReady(), isFalse, reason: '框架在、行还没渲染 → 必须继续等');
      expect(snapshot.diagnostics, contains('课程行数=0'));
    });

    test('行数 > 0 且其它条件满足 → 就绪', () {
      final CaptureSnapshot snapshot = CaptureSnapshot(
        html: _tableWithRows,
        title: '我的课表',
        tries: 2,
        atMs: 500,
        rowCount: countTableRowsInHtml(_tableWithRows),
      );
      expect(snapshot.rowCount, greaterThan(0));
      expect(snapshot.isReady(minLength: 50), isTrue);
    });

    test('轮询：行数 0 → 行数 > 0 才返回就绪，且保留两条快照', () async {
      int calls = 0;
      final CaptureWaitResult result = await waitForTimetableReady(
        interval: const Duration(milliseconds: 1),
        timeout: const Duration(seconds: 5),
        html: () async => _tableWithRows,
        minLength: 50,
        meta: () async => (url: 'https://one.ynau.edu.cn/#/index5', title: '我的课表'),
        rows: () async {
          calls += 1;
          return calls < 2 ? 0 : 3; // 第一次行数为 0（还没渲染），第二次 3 行
        },
      );
      expect(result.ready, isTrue);
      expect(calls, 2);
      expect(result.snapshots.first.rowCount, 0);
      expect(result.snapshots.last.rowCount, 3);
    });

    test('超时仍 0 行 → 失败消息点名"课程行还没渲染"', () async {
      final CaptureWaitResult result = await waitForTimetableReady(
        interval: const Duration(milliseconds: 1),
        timeout: const Duration(milliseconds: 5),
        html: () async => _tableWithRows,
        minLength: 50,
        meta: () async => (url: 'https://one.ynau.edu.cn/#/index5', title: '我的课表'),
        rows: () async => 0,
      );
      expect(result.ready, isFalse);
      expect(result.failureMessage, contains('课程行还没渲染'));
      expect(result.failureMessage, contains('课程行数=0'));
    });
  });

  group('PARSE_EMPTY 重试（t151）', () {
    test('首发解析 0 门 → 重试后成功，诊断记录每次行数与 PARSE_EMPTY', () async {
      CampusCaptureCache.instance.clear();
      CampusCaptureCache.instance.rememberTemplate(
        cardId: '95c8b1e2f3a4b5c6d7e8f9a0b1c2ac18',
        years: '2026-2027-1',
        now: DateTime(2026, 9, 1, 9, 33),
      );
      int imports = 0;
      final ParseRetryResult result = await importHtmlWithParseRetry(
        delay: const Duration(milliseconds: 1),
        fetchHtml: () async => _tableWithRows,
        rowCountOf: countTableRowsInHtml,
        importHtml: (String html) async {
          imports += 1;
          return ImportOutcome(
            term: '2026-2027-1',
            imported: imports == 1 ? 0 : 3, // 第一次解析 0 门（PARSE_EMPTY），第二次成功
            warnings: const <String>[],
          );
        },
      );
      expect(imports, 2);
      expect(result.ok, isTrue);
      expect(result.outcome.imported, 3);
      expect(result.attempts.length, 2);
      expect(result.attempts.first.parseEmpty, isTrue);
      expect(result.attempts.last.parseEmpty, isFalse);
      expect(result.diagnostics, contains('PARSE_EMPTY=是'));
      expect(result.diagnostics, contains('课程行数=3'));
      // t151：诊断里带 cardId（缓存，脱敏）
      expect(result.diagnostics, contains('cardId: 95c8b1…ac18'));
      CampusCaptureCache.instance.clear();
    });

    test('三次都 PARSE_EMPTY → 返回失败并含三条诊断', () async {
      int imports = 0;
      final ParseRetryResult result = await importHtmlWithParseRetry(
        delay: const Duration(milliseconds: 1),
        fetchHtml: () async => _tableWithRows,
        rowCountOf: countTableRowsInHtml,
        importHtml: (String html) async {
          imports += 1;
          return const ImportOutcome(term: '2026-2027-1', imported: 0, warnings: <String>[]);
        },
      );
      expect(imports, 3, reason: '最多 3 次（首发 + 2 次重试）');
      expect(result.ok, isFalse);
      expect(result.attempts.length, 3);
      expect(result.diagnostics, contains('第 1 次'));
      expect(result.diagnostics, contains('第 2 次'));
      expect(result.diagnostics, contains('第 3 次'));
      expect(result.diagnostics, contains('PARSE_EMPTY=是'));
    });

    test('首发即成功 → 不重试', () async {
      int imports = 0;
      final ParseRetryResult result = await importHtmlWithParseRetry(
        fetchHtml: () async => _tableWithRows,
        importHtml: (String html) async {
          imports += 1;
          return const ImportOutcome(term: '2026-2027-1', imported: 8, warnings: <String>[]);
        },
      );
      expect(imports, 1);
      expect(result.ok, isTrue);
      expect(result.attempts.length, 1);
    });
  });

  testWidgets('抓取成功后的提示行包含 cardId（缓存）', (WidgetTester tester) async {
    // 这里只验证"诊断文案拼装"这一层：cardId 由缓存提供，未被抓取时显示"尚未抓取过"。
    CampusCaptureCache.instance.clear();
    expect(describeCachedCardId(), contains('尚未抓取过'));
    CampusCaptureCache.instance.rememberTemplate(
      cardId: '95c8b1e2f3a4b5c6d7e8f9a0b1c2ac18',
      years: '2026-2027-1',
      now: DateTime(2026, 9, 1, 9, 33),
    );
    expect(describeCachedCardId(), contains('95c8b1…ac18'));
    expect(describeCachedCardId(), contains('缓存于 09:33'));
    CampusCaptureCache.instance.clear();
  });
}
