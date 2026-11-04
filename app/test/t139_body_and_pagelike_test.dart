// t139：500 的 JSON 响应体展示、页内同款请求形状、CAS 探针不卡死。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/campus/session_cookie_diagnostics.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';

class _Json500Client implements ProbeClient {
  final List<Uri> urls = <Uri>[];
  final List<Map<String, String>> headers = <Map<String, String>>[];

  @override
  Future<ProbeResponse> get({
    required Uri url,
    required Map<String, String> headers,
    required Duration timeout,
  }) async {
    urls.add(url);
    this.headers.add(headers);
    return const ProbeResponse(
      statusCode: 500,
      contentType: 'application/json;charset=UTF-8',
      body: '{"code":500,"message":"缺少参数 cardId","data":null}',
    );
  }
}

void main() {
  group('响应体展示（t139）', () {
    test('非 2xx 的 JSON → 展示前 300 字符', () {
      final String snippet = jsonErrorSnippet(
        '{"code":500,"message":"缺少参数 cardId"}',
        'application/json;charset=UTF-8',
        500,
      )!;
      expect(snippet, contains('缺少参数 cardId'));
    });

    test('HTML 一律不展示正文；2xx 也不展示', () {
      expect(jsonErrorSnippet('<html>err</html>', 'text/html', 500), isNull);
      expect(jsonErrorSnippet('{"ok":1}', 'application/json', 200), isNull);
    });

    test('超长 JSON 截断到 max', () {
      final String body = '{"a":"${'x' * 500}"}';
      expect(jsonErrorSnippet(body, 'application/json', 500)!.length, 300);
    });

    test('探针结果带上 snippet（500 JSON）', () async {
      final result = await runSessionProbe(
        url: kProbeDefaultUrl,
        userAgent: 'UA',
        cookies: const <ProbeCookie>[ProbeCookie('S', 'v')],
        client: _Json500Client(),
      );
      expect(result.withCookie.bodySnippet, contains('缺少参数 cardId'));
      expect(result.anonymous.bodySnippet, contains('缺少参数 cardId'));
    });
  });

  group('页内同款请求（t139）', () {
    test('URL 形状与页内脚本一致：years/results/cardId/_t，顺序固定', () {
      final Uri url = buildScheduleApiUrl(
        origin: 'https://one.ynau.edu.cn',
        years: '2026-2027-1',
        week: 3,
        cardId: 'a' * 32,
        timestampMs: 1700000000000,
      );
      expect(url.path, kScheduleApiPath);
      expect(
        url.query,
        'years=2026-2027-1&results=3&cardId=${'a' * 32}&_t=1700000000000',
      );
    });

    test('Accept 头与页内一致', () {
      expect(kScheduleAcceptHeader, 'application/json, text/plain, */*');
    });

    test('探针按传入的 accept 发请求', () async {
      final client = _Json500Client();
      await runSessionProbe(
        url: kProbeDefaultUrl,
        userAgent: 'UA',
        cookies: const <ProbeCookie>[],
        client: client,
        accept: kScheduleAcceptHeader,
      );
      expect(client.headers.first['Accept'], kScheduleAcceptHeader);
    });
  });

  group('CAS 探针（t139 不卡死）', () {
    test('returnUrl 用于结束后回到原页面', () {
      CasTicketProbe.returnUrl = 'https://one.ynau.edu.cn/#/index';
      expect(CasTicketProbe.returnUrl, contains('one.ynau.edu.cn'));
      CasTicketProbe.returnUrl = null;
    });

    test('实时链：recording 期间每跳追加，summarize 能给部分结果', () {
      CasTicketProbe.start();
      CasTicketProbe.record('https://ssoo.ynau.edu.cn/cas/login?service=x');
      expect(CasTicketProbe.redirects.length, 1);
      final CasTicketResult partial = CasTicketResult.summarize(List<String>.of(CasTicketProbe.redirects));
      expect(partial.hops, 1);
      expect(partial.ticketFound, isFalse);
      CasTicketProbe.stop();
      CasTicketProbe.redirects.clear();
    });

    test('ticket 仍只保留后 4 位（判读与遮蔽不变）', () {
      final CasTicketResult result = CasTicketResult.summarize(<String>[
        'https://one.ynau.edu.cn/?ticket=ST-9876543210-xyz',
      ]);
      expect(result.prefix, '9876');
      expect(result.finalUrl.contains('ticket=ST-9876****'), isTrue);
    });
  });

  testWidgets('弹窗：默认开启「使用页内同款请求」并显示 CAS 控件', (WidgetTester tester) async {
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SessionProbeDialog(
          cookieLoader: () async => const <ProbeCookie>[ProbeCookie('S', 'v')],
          client: _Json500Client(),
          cookieRefSource: (Uri url) async => const <CookieRef>[],
          jsProbe: () async => 0,
          // 导航慢一点返回，才能观察到"进行中"状态；50ms 后自动完成避免卡住测试
          navigate: (String url) async =>
              Future<void>.delayed(const Duration(milliseconds: 50)),
        ),
      ),
    ));
    await tester.pumpAndSettle();

    await tester.ensureVisible(find.byKey(const Key('probe-page-like')));
    expect(find.byKey(const Key('probe-page-like')), findsOneWidget);
    expect(find.byKey(const Key('probe-years')), findsOneWidget);
    expect(find.byKey(const Key('probe-cardid')), findsOneWidget);

    await tester.ensureVisible(find.byKey(const Key('probe-cas')));
    await tester.tap(find.byKey(const Key('probe-cas')));
    await tester.pump(const Duration(milliseconds: 10)); // 进入"进行中"，导航还没返回
    expect(find.byKey(const Key('probe-cas-cancel')), findsOneWidget);
    await tester.tap(find.byKey(const Key('probe-cas-cancel')));
    await tester.pumpAndSettle();
  });
}
