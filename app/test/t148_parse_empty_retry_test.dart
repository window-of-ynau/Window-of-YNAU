// t148（t161 精简）：PARSE_EMPTY 接线 —— 空壳重取、重取失败报 HTML_CHANGED、合格不重取、抓取入口幂等。
//
// 探针相关用例（env 排查）已随调试设施一起下线（t161 / issue #55）。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_bridge.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/core/timetable_service.dart';

final String _goodHtml =
    '<html><title>我的课表</title><table id="timetable"><tr><td>高等数学</td></tr></table>'
    '${'x' * 2400}</html>';

void main() {
  group('PARSE_EMPTY 接线（t148）', () {
    test('第一次空壳 → bridge 调一次重取 → 最终成功且诊断是第 2 次', () async {
      final List<FetchedPage> fetched = <FetchedPage>[];
      final List<FetchException> errors = <FetchException>[];
      int grabCalls = 0;
      final CampusBridge bridge = CampusBridge(
        onFetched: fetched.add,
        onError: errors.add,
        pollInterval: const Duration(milliseconds: 1),
        pollTimeout: const Duration(seconds: 5),
        grabHtml: () async {
          grabCalls += 1;
          return _goodHtml;
        },
      );

      bridge.handle(<String, dynamic>{'type': 'timetable', 'html': '<html></html>', 'path': '/x'});
      await Future<void>.delayed(const Duration(milliseconds: 30));
      expect(grabCalls, 1);
      expect(errors, isEmpty);
      expect(fetched.length, 1);
      expect(fetched.single.html, _goodHtml);
      expect(bridge.lastHtmlDiagnostics, contains('（第 2 次）'));
      expect(bridge.lastHtmlDiagnostics, contains('是否含课表关键字=true'));
    });

    test('重取仍是空壳 → 才报 HTML_CHANGED，且诊断含每次结果', () async {
      final List<FetchException> errors = <FetchException>[];
      final CampusBridge bridge = CampusBridge(
        onFetched: (_) {},
        onError: errors.add,
        pollInterval: const Duration(milliseconds: 1),
        pollTimeout: const Duration(milliseconds: 20),
        grabHtml: () async => '<html></html>',
      );
      bridge.handle(<String, dynamic>{'type': 'timetable', 'html': '<html></html>'});
      await Future<void>.delayed(const Duration(milliseconds: 30));
      expect(errors.length, 1);
      expect(errors.single.code, 'HTML_CHANGED');
      expect(errors.single.message, contains('html 长度='));
      expect(errors.single.message, contains('首次：'));
    });

    test('第一次就合格 → 不触发重取', () async {
      int grabCalls = 0;
      final List<FetchedPage> fetched = <FetchedPage>[];
      final CampusBridge bridge = CampusBridge(
        onFetched: fetched.add,
        onError: (_) {},
        grabHtml: () async {
          grabCalls += 1;
          return _goodHtml;
        },
      );
      bridge.handle(<String, dynamic>{'type': 'timetable', 'html': _goodHtml});
      await Future<void>.delayed(const Duration(milliseconds: 5));
      expect(grabCalls, 0);
      expect(fetched.length, 1);
      expect(bridge.lastHtmlDiagnostics, contains('（第 1 次）'));
    });

    test('页面侧抓取入口：幂等安装 + 不含网络调用（功能必需，t161 保留）', () {
      expect(kGrabHtmlInstallScript.contains('window.__yunongGrabHtml'), isTrue);
      expect(kGrabHtmlInstallScript.contains('if (window.__yunongGrabHtml) { return; }'), isTrue,
          reason: '必须幂等，重复注入不覆盖');
      expect(kGrabHtmlInstallScript.contains('fetch('), isFalse);
      expect(kGrabHtmlInstallScript.contains('XMLHttpRequest'), isFalse);
      // t151 的课程行计数也保留（属于就绪判定，不是调试设施）
      expect(kCourseRowSelector, 'table#timetable tr');
      expect(kCourseRowCountInstallScript.contains('window.__yunongCourseRowCount'), isTrue);
    });
  });
}
