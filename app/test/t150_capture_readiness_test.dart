// t150：PARSE_EMPTY 的「轮询等待就绪 + 就绪诊断」与 cardId 缓存。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_bridge.dart';
import 'package:yunong/src/core/timetable_service.dart';
import 'package:yunong/src/campus/capture_cache.dart';
import 'package:yunong/src/campus/capture_readiness.dart';

final String _timetableHtml =
    '<html><title>我的课表</title><table id="timetable"><tr><td>课程 节次 周次</td></tr></table>'
    '${'x' * 1200}</html>';
const String _loginHtml = '<html><title>统一身份认证</title>请先登录</html>';

void main() {
  group('就绪诊断（t150）', () {
    test('登录页快照：能被识别为"登录页"且不是就绪', () {
      const CaptureSnapshot snapshot = CaptureSnapshot(
        html: _loginHtml,
        url: 'https://ssoo.ynau.edu.cn/cas/login',
        title: '统一身份认证',
        tries: 1,
        atMs: 0,
      );
      expect(snapshot.looksLikeLoginPage, isTrue);
      expect(snapshot.isReady(), isFalse);
      expect(snapshot.diagnostics, contains('含登录字样=true'));
      expect(snapshot.diagnostics, contains('html 长度='));
      expect(snapshot.diagnostics, contains('标题="统一身份认证"'));
      expect(snapshot.diagnostics, contains('URL="https://ssoo.ynau.edu.cn/cas/login"'));
    });

    test('轮询：先登录页、第 3 次才就绪 → 返回就绪且保留三次诊断', () async {
      int calls = 0;
      final CaptureWaitResult result = await waitForTimetableReady(
        interval: const Duration(milliseconds: 1),
        timeout: const Duration(seconds: 5),
        html: () async {
          calls += 1;
          return calls < 3 ? _loginHtml : _timetableHtml;
        },
        meta: () async => (
          url: calls < 3 ? 'https://ssoo.ynau.edu.cn/cas/login' : 'https://one.ynau.edu.cn/#/index5',
          title: calls < 3 ? '统一身份认证' : '我的课表',
        ),
      );
      expect(result.ready, isTrue);
      expect(calls, 3);
      expect(result.snapshots.length, 3);
      expect(result.snapshots.first.looksLikeLoginPage, isTrue);
      expect(result.last.hasTimetableMark, isTrue);
    });

    test('超时仍未就绪 → 失败消息点名"登录未完成"并带 URL/标题/长度', () async {
      final CaptureWaitResult result = await waitForTimetableReady(
        interval: const Duration(milliseconds: 1),
        timeout: const Duration(milliseconds: 5),
        html: () async => _loginHtml,
        meta: () async => (url: 'https://ssoo.ynau.edu.cn/cas/login', title: '统一身份认证'),
      );
      expect(result.ready, isFalse);
      expect(result.failureMessage, contains('登录未完成'));
      expect(result.failureMessage, contains('URL="https://ssoo.ynau.edu.cn/cas/login"'));
      expect(result.failureMessage, contains('标题="统一身份认证"'));
      expect(result.failureMessage, contains('html 长度='));
    });
  });

  group('bridge 抓取路径（t150）', () {
    test('登录页 HTML → 报 NOT_LOGGED_IN（不是 HTML_CHANGED/PARSE_EMPTY）', () async {
      final List<FetchException> errors = <FetchException>[];
      final CampusBridge bridge = CampusBridge(
        onFetched: (_) {},
        onError: errors.add,
        pollInterval: const Duration(milliseconds: 1),
        pollTimeout: const Duration(milliseconds: 5),
        grabHtml: () async => _loginHtml,
        metaProbe: () async => (url: 'https://ssoo.ynau.edu.cn/cas/login', title: '统一身份认证'),
      );
      bridge.handle(<String, dynamic>{'type': 'timetable', 'html': _loginHtml});
      await Future<void>.delayed(const Duration(milliseconds: 40));

      expect(errors.length, 1);
      expect(errors.single.code, 'NOT_LOGGED_IN');
      expect(errors.single.userHint, contains('登录'));
      expect(errors.single.message, contains('登录'));
      expect(errors.single.message, contains('URL='));
    });

    test('首次空壳、轮询后拿到课表 → 成功交付', () async {
      int grab = 0;
      final List<FetchedPage> fetched = <FetchedPage>[];
      final CampusBridge bridge = CampusBridge(
        onFetched: fetched.add,
        onError: (_) {},
        pollInterval: const Duration(milliseconds: 1),
        pollTimeout: const Duration(seconds: 5),
        minHtmlLength: 0,
        grabHtml: () async {
          grab += 1;
          return grab < 2 ? _loginHtml : _timetableHtml;
        },
        metaProbe: () async => (url: 'https://one.ynau.edu.cn/#/index5', title: '我的课表'),
      );
      bridge.handle(<String, dynamic>{'type': 'timetable', 'html': '<html></html>'});
      await Future<void>.delayed(const Duration(milliseconds: 60));
      expect(fetched.length, 1);
      expect(fetched.single.html, _timetableHtml);
    });

    test('抓到 schedule 模板 → 写入 cardId 缓存', () async {
      CampusCaptureCache.instance.clear();
      final CampusBridge bridge = CampusBridge(onFetched: (_) {}, onError: (_) {});
      bridge.handle(<String, dynamic>{
        'type': 'schedule_template',
        'card_id': '0123456789abcdef0123456789abcdef',
        'years': '2026-2027-1',
      });
      expect(CampusCaptureCache.instance.hasCardId, isTrue);
      expect(CampusCaptureCache.instance.entry!.cardId, '0123456789abcdef0123456789abcdef');
      expect(describeCachedCardId(), contains('来自上次抓取'));
      CampusCaptureCache.instance.clear();
    });
  });

}
