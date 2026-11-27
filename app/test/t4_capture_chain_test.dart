// #53/#54：抓取链路收尾的回归 —— PARSE_EMPTY 归因 + 页面脚本静默错误可见。
//
// ① 用「复制诊断信息」要能一眼回答：PARSE_EMPTY 是「数据行=0」还是「行数>0 解析 0 门」。
// ② portal_api 的关键路径异常（hook/桥接/探测解析/按周取数）不再 `catch(e){}` 吞掉，
//    统一进 `silentErrors` 并在失败详情里可见。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_bridge.dart';
import 'package:yunong/src/campus/capture_log.dart';
import 'package:yunong/src/campus/capture_readiness.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/core/timetable_service.dart';

void main() {
  group('#53 PARSE_EMPTY 归因', () {
    test('真实样本：只有表头行（<th>节次</th>）→ 数据行=0，属“抓早了/空壳”', () {
      // 取自 yunong-dev/logs/capture-20261107-190545.log 的 HTML 片段。
      const String html = '<html><head><title>我的课表</title></head><body>'
          '<table id="timetable"><tr><th>节次</th></tr></table></body></html>';
      final ({int dataRows, int headerRows, int totalRows}) rc = tableRowCounts(html);
      expect(rc.dataRows, 0);
      expect(rc.headerRows, 1);

      final String cause = classifyParseEmpty(<CaptureAttemptRecord>[
        CaptureAttemptRecord(
          index: 1,
          htmlLength: html.length,
          rowCount: rc.dataRows,
          headerRowCount: rc.headerRows,
          parsedCourses: 0,
        ),
      ]);
      expect(cause, contains('只有 1 行表头'));
      expect(cause, contains('抓早了'));
      expect(cause, contains('非解析器问题'));
    });

    test('数据行=0 且无表头 → 选择器/表结构变了', () {
      final String cause = classifyParseEmpty(<CaptureAttemptRecord>[
        const CaptureAttemptRecord(index: 1, htmlLength: 1250, rowCount: 0, headerRowCount: 0),
      ]);
      expect(cause, contains('选择器/表结构变了'));
    });

    test('数据行>0 但解析 0 门 → 解析器与页面不匹配', () {
      final String cause = classifyParseEmpty(<CaptureAttemptRecord>[
        const CaptureAttemptRecord(
          index: 1,
          htmlLength: 8000,
          rowCount: 7,
          headerRowCount: 1,
          parsedCourses: 0,
        ),
      ]);
      expect(cause, contains('数据行=7'));
      expect(cause, contains('解析器与页面不匹配'));
    });

    test('复制诊断正文包含 PARSE_EMPTY 归因行', () {
      final String text = CaptureLog.buildDiagnostics(
        code: 'PARSE_EMPTY',
        userHint: '课表解析出 0 门课',
        message: '第 1 次：html 长度=1250 / 数据行=0 / 解析出=0 门课',
        attempts: const <CaptureAttemptRecord>[
          CaptureAttemptRecord(
            index: 1,
            htmlLength: 1250,
            rowCount: 0,
            headerRowCount: 1,
            parsedCourses: 0,
          ),
        ],
      );
      expect(text, contains('--- PARSE_EMPTY 归因 ---'));
      expect(text, contains('只有 1 行表头'));
    });
  });

  group('#54 页面脚本关键路径静默错误上报', () {
    test('kPortalScheduleScript 内关键 catch 走 noteError，并上报 silentErrors', () {
      for (final String needle in <String>[
        'function noteError(',
        'silentErrors',
        "noteError('hook.fetch'",
        "noteError('hook.xhr'",
        "noteError('observer'",
        "'week.' + week",
        "':0:badjson'",
        'silentErrors: DIAG.silentErrors || []',
      ]) {
        expect(kPortalScheduleScript.contains(needle), isTrue,
            reason: '缺少关键路径上报标记：$needle');
      }
      // t12 F9：探测 JSON 解析失败是噪音，不得进 silentErrors。
      expect(kPortalScheduleScript.contains("noteError('probe.json'"), isFalse);
    });

    test('CampusBridge 失败详情显示“静默错误”（用户可复制）', () {
      FetchException? failure;
      final CampusBridge bridge = CampusBridge(
        onFetched: (FetchedPage page) {},
        onError: (FetchException error) => failure = error,
      );
      final bool handled = bridge.handle(<String, dynamic>{
        'type': 'capture_timeout',
        'diagnostics': <String, dynamic>{
          'silentErrors': <dynamic>[
            'hook.fetch:Error:cannot read url',
            'week.3:Error:timeout',
          ],
        },
      });
      expect(handled, isTrue);
      expect(failure, isNotNull);
      expect(failure!.code, 'CAPTURE_TIMEOUT');
      expect(failure!.message, contains('静默错误'));
      expect(failure!.message, contains('hook.fetch'));
      expect(failure!.message, contains('week.3'));
    });
  });
}
