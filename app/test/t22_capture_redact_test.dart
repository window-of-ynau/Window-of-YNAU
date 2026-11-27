// security R7 M1/M2 回归：诊断日志 / 「复制诊断」/ 成功日志都必须脱敏。
//
// 4 条用例分别覆盖：
// 1) sanitize 规则本身（学号 / 姓名 / cardId / token）；
// 2) 失败日志（整页 HTML 回退路径）不出现原值；
// 3) CaptureApiRecord.line 自身 + buildDiagnostics（复制诊断）不出现完整 cardId；
// 4) capture-success.log 同样走 sanitize。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/capture_log.dart';
import 'package:yunong/src/campus/redact.dart';
import 'package:yunong/src/core/app_paths.dart';

const String _cardId = 'd907dd7012345678abcdef0123456789';

void main() {
  late Directory temp;

  setUp(() {
    temp = Directory.systemTemp.createTempSync('yunong_t22_');
    AppPaths.initDataRoot(
      env: const <String, String>{},
      args: <String>['--data-dir=${temp.path}'],
    );
  });

  tearDown(() {
    AppPaths.initDataRoot(env: const <String, String>{}, args: const <String>[]);
    try {
      if (temp.existsSync()) {
        temp.deleteSync(recursive: true);
      }
    } on Object {
      // 清理失败不影响结论
    }
  });

  test('M1/M2：sanitize 掩码学号 / 姓名 / cardId / token', () {
    const String raw = '学号：20231122 姓名：张三 '
        'cardId=$_cardId studentId=20231123 name="李四" '
        'access_token="a1b2c3d4e5f6" ticket=ST-123456';

    final String out = CaptureLog.sanitize(raw);

    for (final String secret in <String>[
      '20231122',
      '20231123',
      '张三',
      '李四',
      _cardId,
      'a1b2c3d4e5f6',
      'ST-123456',
    ]) {
      expect(out, isNot(contains(secret)), reason: '泄露：$secret\n$out');
    }
    // 仍可定位：学号保留后 4 位、cardId 保留前 6/后 4、姓名给固定占位。
    expect(out, contains('学号：****1122'));
    expect(out, contains('studentId=****1123'));
    expect(out, contains('d907dd…6789'));
    expect(out, contains('姓名：<已脱敏>'));
    expect(out, contains('name=<已脱敏>'));
    expect(out, contains('access_token=<已脱敏>'));
  });

  test('M1：整页 HTML 回退的失败日志不出现学号/姓名/cardId', () {
    final String html = '<html><body>'
        '<div class="top">学号：20231122 姓名：张三 data-cardid="$_cardId"</div>'
        '${'x' * 2500}'
        '<div class="bottom">学号：20231122 姓名：张三 cardId=$_cardId</div>'
        '</body></html>';

    final CaptureWriteResult write = CaptureLog.writeFailure(
      code: 'PARSE_EMPTY',
      html: html,
      userHint: '课表解析出 0 门课',
      message: '学号：20231122 姓名：张三',
      attempts: const <CaptureAttemptRecord>[
        CaptureAttemptRecord(index: 1, htmlLength: 3000, rowCount: 0),
      ],
    );
    expect(write.path, isNotNull);
    final String body = File(write.path!).readAsStringSync();

    for (final String secret in <String>['20231122', '张三', _cardId]) {
      expect(body, isNot(contains(secret)), reason: '日志泄露：$secret');
    }
    expect(body, contains('学号：****1122'));
    expect(body, contains('姓名：<已脱敏>'));
    expect(body, contains('d907dd…6789'));
  });

  test('M2：CaptureApiRecord.line 与「复制诊断」不含完整 cardId', () {
    const CaptureApiRecord api = CaptureApiRecord(
      jsonLength: 128,
      elementsCount: 0,
      yearsCount: 1,
      term: '2026-2027-1',
      url: 'https://one.ynau.edu.cn/api?cardId=$_cardId&studentId=20231122',
      cardId: _cardId,
    );
    // 源头就掩码（不再依赖出口 sanitize 兜底）
    expect(api.line, isNot(contains(_cardId)));
    expect(api.line, contains('d907dd…6789'));

    final String copy = CaptureLog.buildDiagnostics(
      code: 'CAPTURE_TIMEOUT',
      userHint: '抓取超时',
      message: '学号：20231122 姓名：张三',
      attempts: const <CaptureAttemptRecord>[
        CaptureAttemptRecord(
          index: 1,
          htmlLength: 1200,
          rowCount: 0,
          url: 'https://one.ynau.edu.cn/api?cardId=d907dd7012345678abcdef0123456789',
        ),
      ],
      api: api,
    );
    expect(copy, isNot(contains(_cardId)));
    expect(copy, isNot(contains('20231122')));
    expect(copy, isNot(contains('张三')));
    expect(copy, contains('学号：****1122'));
    expect(copy, contains('d907dd…6789'));
  });

  test('M1/M2：capture-success.log 同样走 sanitize', () {
    CaptureLog.writeSuccess(
      htmlLength: 2048,
      rowCount: 1,
      title: '姓名：张三',
      url: 'https://one.ynau.edu.cn/api?cardId=$_cardId&studentId=20231122',
      parsedCourses: 0,
    );
    final File file = File(CaptureLog.successPath());
    expect(file.existsSync(), isTrue);
    final String line = file.readAsStringSync();

    for (final String secret in <String>['20231122', '张三', _cardId]) {
      expect(line, isNot(contains(secret)), reason: '成功日志泄露：$secret');
    }
    expect(line, contains('d907dd…6789'));
    expect(line, contains('****1122'));
    expect(line, contains('姓名：<已脱敏>'));

    // 掩码规则与 capture_cache 同口径（共用 redact.dart）
    expect(maskIdentifier(_cardId), 'd907dd…6789');
    expect(maskStudentId('20231122'), '****1122');
  });
}
