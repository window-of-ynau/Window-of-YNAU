// t176：接口路径失败/空数据也必须写日志；接口 0 门课自动回退 DOM；写失败要在界面可见。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/capture_log.dart';
import 'package:yunong/src/campus/timetable_page.dart';
import 'package:yunong/src/core/app_paths.dart';

void main() {
  late Directory temp;

  setUp(() {
    temp = Directory.systemTemp.createTempSync('yunong_t176_');
    AppPaths.initDataRoot(
      env: const <String, String>{},
      args: <String>['--data-dir=${temp.path}'],
    );
  });

  tearDown(() {
    if (temp.existsSync()) {
      temp.deleteSync(recursive: true);
    }
  });

  group('接口路径也要写日志（t176 #1）', () {
    test('elements=[] → 写日志（路径=api + json 长度 + elements 数量）', () {
      const CaptureApiRecord api = CaptureApiRecord(
        jsonLength: 42,
        jsonHead: '{"term":"2026-2027-1","weeks":[],"years":""}',
        elementsCount: 0,
        yearsCount: 0,
        term: '2026-2027-1',
        url: 'https://one.ynau.edu.cn/api/x',
        imported: null,
      );
      final CaptureWriteResult write = CaptureLog.writeFailure(
        code: 'API_EMPTY',
        html: '',
        attempts: const <CaptureAttemptRecord>[],
        userHint: '接口返回 0 门课',
        message: '接口 JSON 解析出 0 门课',
        path: 'api',
        api: api,
      );
      expect(write.ok, isTrue, reason: write.error ?? '');
      final String body = File(write.path!).readAsStringSync();
      expect(body, contains('路径=api'));
      expect(body, contains('json 长度=42'));
      expect(body, contains('elements=0 个'));
      expect(body, contains('years=0 个'));
      expect(body, contains('## 接口路径'));
      expect(body, contains('json 前 500 字符'));
    });

    test('接口 0 门课 → 决策 = 回退 DOM（不再直接把错误甩给用户）', () {
      expect(
        decideAfterApi(apiSucceeded: true, imported: 0),
        TimetableSourceDecision.fallbackToDom,
      );
      expect(
        decideAfterApi(apiSucceeded: false, imported: 0, errorCode: 'API_EMPTY'),
        TimetableSourceDecision.fallbackToDom,
      );
      expect(
        decideAfterApi(apiSucceeded: false, imported: 0, errorCode: 'CARD_NOT_FOUND'),
        TimetableSourceDecision.fallbackToDom,
      );
      // 有课 → 用接口；其它接口错误 → 直接报错（该路径的日志已在上面写过）
      expect(
        decideAfterApi(apiSucceeded: true, imported: 12),
        TimetableSourceDecision.useApi,
      );
      expect(
        decideAfterApi(apiSucceeded: false, imported: 0, errorCode: 'NETWORK'),
        TimetableSourceDecision.fail,
      );
    });
  });

  group('两条路径都失败时日志里都有（t176 #2）', () {
    test('DOM 路径日志同时带接口路径记录与接口日志路径', () {
      const CaptureApiRecord api = CaptureApiRecord(
        jsonLength: 7,
        elementsCount: 0,
        yearsCount: 0,
        term: '2026-2027-1',
        url: 'https://one.ynau.edu.cn/api/x',
      );
      final CaptureWriteResult write = CaptureLog.writeFailure(
        code: 'PARSE_EMPTY',
        html: '<html><table id="timetable"><tr><th>节次</th></tr></table></html>',
        attempts: const <CaptureAttemptRecord>[
          CaptureAttemptRecord(
            index: 1,
            htmlLength: 70,
            rowCount: 0,
            headerRowCount: 1,
            parsedCourses: 0,
            hasTimetableMark: true,
          ),
        ],
        userHint: '课表解析出 0 门课',
        message: 'dom 诊断',
        path: 'dom',
        api: api,
        apiLogPath: 'C:/tmp/capture-api.log',
      );
      expect(write.ok, isTrue, reason: write.error ?? '');
      final String body = File(write.path!).readAsStringSync();
      expect(body, contains('路径=dom'));
      expect(body, contains('路径=api'));
      expect(body, contains('接口路径日志：C:/tmp/capture-api.log'));
      expect(body, contains('数据行=0'));
      expect(body, contains('解析出=0 门课'));
    });
  });

  group('日志写不出来必须可见（t176 #3）', () {
    test('目录不可写 → CaptureWriteResult 带 error，describe 明确写「日志写入失败」', () {
      // 用一个"父亲是文件"的路径让 createSync 必然失败（跨平台稳定）
      final File blocker = File('${temp.path}${Platform.pathSeparator}blocked')
        ..writeAsStringSync('x');
      AppPaths.initDataRoot(
        env: const <String, String>{},
        args: <String>['--data-dir=${blocker.path}${Platform.pathSeparator}sub'],
      );
      final CaptureWriteResult write = CaptureLog.writeFailure(
        code: 'PARSE_EMPTY',
        html: '<html></html>',
        attempts: const <CaptureAttemptRecord>[],
        userHint: 'hint',
        message: 'msg',
      );
      expect(write.ok, isFalse);
      expect(write.error, isNotNull);
      expect(write.describe, startsWith('日志写入失败：'));
    });

    test('复制诊断信息：包含错误码、接口特征与日志路径', () {
      const CaptureApiRecord api = CaptureApiRecord(
        jsonLength: 3,
        elementsCount: 0,
        yearsCount: 1,
        term: '2026-2027-1',
        url: 'https://one.ynau.edu.cn/api/x',
      );
      final String text = CaptureLog.buildDiagnostics(
        code: 'PARSE_EMPTY',
        userHint: '课表解析出 0 门课',
        message: 'raw',
        attempts: const <CaptureAttemptRecord>[
          CaptureAttemptRecord(index: 1, htmlLength: 1234, rowCount: 0, parsedCourses: 0),
        ],
        api: api,
        writeResult: const CaptureWriteResult(error: 'PathAccessException'),
      );
      expect(text, contains('错误码：PARSE_EMPTY'));
      expect(text, contains('日志写入失败：PathAccessException'));
      expect(text, contains('--- 接口路径 ---'));
      expect(text, contains('elements=0 个'));
      expect(text, contains('--- 尝试 ---'));
      expect(text, contains('html 长度=1234'));
    });
  });
}
