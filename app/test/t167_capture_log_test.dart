// t167：失败诊断落盘 + 短消息/详情面板 + 成功结构特征记录。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/capture_log.dart';
import 'package:yunong/src/core/app_paths.dart';
import 'package:yunong/src/core/timetable_service.dart';

void main() {
  late Directory temp;

  setUp(() {
    temp = Directory.systemTemp.createTempSync('yunong_t167_');
    // 把数据根指到临时目录（覆盖数据根会让 logs 跟着走）
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

  test('失败日志：落在 <数据根>/logs，含每次尝试特征与 HTML 首尾片段', () {
    final String html =
        '<html><head><title>我的课表</title></head><body><table id="timetable">'
        '${'<tr><td>课</td></tr>' * 300}'
        '</table></body></html>';
    final String? path = CaptureLog.writeFailure(
      code: 'PARSE_EMPTY',
      html: html,
      userHint: '课表解析出 0 门课',
      message: '第 1 次：html 长度=…；第 2 次：…',
      attempts: const <CaptureAttemptRecord>[
        CaptureAttemptRecord(index: 1, htmlLength: 12000, rowCount: 0, url: 'https://one.ynau.edu.cn/#/index5'),
        CaptureAttemptRecord(
          index: 2,
          htmlLength: 12000,
          rowCount: 3,
          url: 'https://one.ynau.edu.cn/#/index5',
          hasTimetableMark: true,
          parsedCourses: 0,
        ),
      ],
    );
    expect(path, isNotNull);
    expect(path, contains('logs'));
    expect(File(path!).existsSync(), isTrue);

    final String body = File(path).readAsStringSync();
    expect(body, contains('错误码：PARSE_EMPTY'));
    expect(body, contains('课程行数=0'));
    expect(body, contains('课程行数=3'));
    expect(body, contains('解析出=0 门课'));
    expect(body, contains('## HTML 前 2000 字符'));
    expect(body, contains('## HTML 后 2000 字符'));
    // 长度上限：首/尾各 2000
    final int headStart = body.indexOf('## HTML 前 2000 字符');
    final int tailStart = body.indexOf('## HTML 后 2000 字符');
    final String head = body.substring(headStart, tailStart);
    expect(head.length, lessThan(2200));
    expect(body.length, lessThan(20000), reason: '日志不能把整份 HTML 写进去');
  });

  test('脱敏：Cookie / token / ticket 的值不落盘', () {
    final String? path = CaptureLog.writeFailure(
      code: 'PARSE_EMPTY',
      html: '<html>Cookie: JSESSIONID=abc123; token=secret-token-value</html>',
      attempts: const <CaptureAttemptRecord>[
        CaptureAttemptRecord(
          index: 1,
          htmlLength: 60,
          url: 'https://one.ynau.edu.cn/#/index5?ticket=ST-123',
        ),
      ],
      message: 'authorization=Bearer abc.def',
    );
    final String body = File(path!).readAsStringSync();
    expect(body, isNot(contains('JSESSIONID=abc123')));
    expect(body, isNot(contains('secret-token-value')));
    expect(body, isNot(contains('ST-123')));
    expect(body, isNot(contains('Bearer abc.def')));
    expect(body, contains('<已脱敏>'));
  });

  test('成功记录：追加一行结构特征', () {
    CaptureLog.writeSuccess(
      htmlLength: 12345,
      rowCount: 7,
      title: '我的课表',
      url: 'https://one.ynau.edu.cn/#/index5',
      parsedCourses: 6,
    );
    final File file = File(CaptureLog.successPath());
    expect(file.existsSync(), isTrue);
    final String line = file.readAsStringSync().trim();
    expect(line, contains('成功'));
    expect(line, contains('html 长度=12345'));
    expect(line, contains('课程行数=7'));
    expect(line, contains('解析出=6 门课'));
    expect(line, contains('标题="我的课表"'));
  });

  test('打开日志目录：平台分支', () {
    expect(CaptureLog.openCommandFor('windows', r'C:\x\logs'), <String>['explorer', r'C:\x\logs']);
    expect(CaptureLog.openCommandFor('macos', '/x/logs'), <String>['open', '/x/logs']);
    expect(CaptureLog.openCommandFor('linux', '/x/logs'), <String>['xdg-open', '/x/logs']);
    expect(CaptureLog.openCommandFor('android', '/x/logs'), isNull);
    expect(CaptureLog.openCommandFor('ios', '/x/logs'), isNull);
  });

  test('失败短消息：每种错误码一句话', () {
    expect(captureFailureHeadline('PARSE_EMPTY'), '抓取失败：页面里没找到课程（详见日志）');
    expect(captureFailureHeadline('NOT_LOGGED_IN'), contains('登录还没完成'));
    expect(captureFailureHeadline('HTML_CHANGED'), contains('页面结构变了'));
    expect(captureFailureHeadline('BRIDGE_UNAVAILABLE'), contains('通道没建立起来'));
    expect(captureFailureHeadline('SOMETHING_ELSE'), isEmpty);
  });

  test('详情面板只取最近 3 次尝试', () {
    final List<ParseAttempt> attempts = <ParseAttempt>[
      for (int i = 1; i <= 5; i++)
        ParseAttempt(index: i, htmlLength: 1000 * i, rowCount: i, parseEmpty: true),
    ];
    final List<String> lines = recentAttemptLines(attempts);
    expect(lines.length, 3);
    expect(lines.first, contains('第 3 次'));
    expect(lines.last, contains('第 5 次'));
  });
}