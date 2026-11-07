// t168 修复：就绪判定必须用「数据行」而不是「表头行」——PARSE_EMPTY 的根因回归。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/capture_log.dart';
import 'package:yunong/src/campus/capture_readiness.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/core/app_paths.dart';

/// 真实教务页形态：第一个 `<tr>` 全是 `<th>`（表头），后面才是含 `<td>` 的课程行。
const String _headerOnlyHtml = '''
<html><head><title>学生课表</title></head><body>
<table id="timetable">
  <tr><th>节次</th><th>星期一</th><th>星期二</th></tr>
</table>
${'<!-- padding -->'}
</body></html>
''';

const String _headerPlusOneRow = '''
<html><head><title>学生课表</title></head><body>
<table id="timetable">
  <tr><th>节次</th><th>星期一</th><th>星期二</th></tr>
  <tr><td>1-2节</td><td>高等数学</td><td></td></tr>
</table>
${'<!-- padding -->'}
</body></html>
''';

void main() {
  group('行数统计：表头 vs 数据行（t168 修复）', () {
    test('只有表头 → 数据行=0，表头=1', () {
      final ({int dataRows, int headerRows, int totalRows}) counts = tableRowCounts(_headerOnlyHtml);
      expect(counts.totalRows, 1);
      expect(counts.headerRows, 1);
      expect(counts.dataRows, 0, reason: '表头行不能算课程行');
      expect(countTableRowsInHtml(_headerOnlyHtml), 0);
    });

    test('真实样本 fixture：表头不计入数据行', () {
      // 样本文件里第一行也是 <th>（与真实教务页一致）
      final String html = File('assets/fixtures/timetable_sample.html').readAsStringSync();
      final ({int dataRows, int headerRows, int totalRows}) counts = tableRowCounts(html);
      expect(counts.headerRows, greaterThanOrEqualTo(1));
      expect(counts.dataRows, greaterThan(0), reason: '样本里有课程行');
      expect(counts.totalRows, counts.dataRows + counts.headerRows);
    });

    test('选择器口径：数据行 = 含 <td> 的行', () {
      expect(kCourseDataRowSelector, 'table#timetable tr:has(td)');
      // 页面脚本用的也是"含 td 才算数据行"
      expect(kCourseRowCountScript.contains('__yunongCourseRowCount'), isTrue);
      expect(kCourseRowCountInstallScript.contains("querySelector('td')"), isTrue);
    });
  });

  group('就绪判定：只有表头 → 继续等待（t168 修复）', () {
    test('只有表头且行数=0 → 未就绪', () {
      final CaptureSnapshot snapshot = CaptureSnapshot(
        html: _headerOnlyHtml,
        title: '学生课表',
        tries: 1,
        atMs: 0,
        rowCount: countTableRowsInHtml(_headerOnlyHtml),
      );
      expect(snapshot.hasTimetableMark, isTrue);
      expect(snapshot.rowCount, 0);
      expect(snapshot.isReady(minLength: 50), isFalse,
          reason: '只有表头时不能判定就绪（否则 Rust 解析 0 门 → PARSE_EMPTY）');
    });

    test('表头 + 1 行数据 → 就绪', () {
      final CaptureSnapshot snapshot = CaptureSnapshot(
        html: _headerPlusOneRow,
        title: '学生课表',
        tries: 2,
        atMs: 500,
        rowCount: countTableRowsInHtml(_headerPlusOneRow),
      );
      expect(snapshot.rowCount, 1);
      expect(snapshot.isReady(minLength: 50), isTrue);
    });

    test('轮询：先只有表头（等待），出现课程行后才就绪', () async {
      int calls = 0;
      final CaptureWaitResult result = await waitForTimetableReady(
        interval: const Duration(milliseconds: 1),
        timeout: const Duration(seconds: 5),
        minLength: 50,
        html: () async => _headerPlusOneRow,
        meta: () async => (url: 'https://one.ynau.edu.cn/#/index5', title: '学生课表'),
        rows: () async {
          calls += 1;
          return calls < 2 ? 0 : 1; // 第一次只有表头
        },
      );
      expect(result.ready, isTrue);
      expect(calls, 2);
      expect(result.snapshots.first.rowCount, 0);
      expect(result.snapshots.last.rowCount, 1);
    });
  });

  group('诊断日志分开记表头行与数据行（t168 修复）', () {
    test('日志行含 表头行=… / 数据行=…', () {
      const CaptureAttemptRecord record = CaptureAttemptRecord(
        index: 1,
        htmlLength: 300,
        rowCount: 0,
        headerRowCount: 1,
        parsedCourses: 0,
      );
      expect(record.line, contains('表头行=1'));
      expect(record.line, contains('数据行=0'));
      expect(record.line, contains('解析出=0 门课'));
    });

    test('写盘日志里也带上两个数（便于 owner 直接看出"只有表头"）', () {
      final Directory temp = Directory.systemTemp.createTempSync('yunong_t168_');
      AppPaths.initDataRoot(env: const <String, String>{}, args: <String>['--data-dir=${temp.path}']);
      try {
        final String? path = CaptureLog.writeFailure(
          code: 'PARSE_EMPTY',
          html: _headerOnlyHtml,
          attempts: const <CaptureAttemptRecord>[
            CaptureAttemptRecord(index: 1, htmlLength: 300, rowCount: 0, headerRowCount: 1),
          ],
        );
        final String body = File(path!).readAsStringSync();
        expect(body, contains('表头行=1'));
        expect(body, contains('数据行=0'));
      } finally {
        AppPaths.initDataRoot(env: const <String, String>{}, args: const <String>[]);
        if (temp.existsSync()) {
          temp.deleteSync(recursive: true);
        }
      }
    });
  });
}