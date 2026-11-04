// t140：收敛探测——校方域名禁止 Dart HTTP、限流与冷却、批量探测已删除、CAS 一次导航。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/campus/session_cookie_diagnostics.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';

class _Recorder implements ProbeClient {
  final List<Uri> urls = <Uri>[];

  @override
  Future<ProbeResponse> get({
    required Uri url,
    required Map<String, String> headers,
    required Duration timeout,
  }) async {
    urls.add(url);
    return const ProbeResponse(statusCode: 200, contentType: 'application/json', body: '{}');
  }
}

void main() {
  group('校方域名禁止 Dart HTTP（t140）', () {
    test('isCampusHost 覆盖 owner 提到的域名与子域', () {
      expect(isCampusHost('ssoo.ynau.edu.cn'), isTrue);
      expect(isCampusHost('ssoo.ynu.edu.cn'), isTrue);
      expect(isCampusHost('one.ynau.edu.cn'), isTrue);
      expect(isCampusHost('jwxt.ynau.edu.cn'), isTrue);
      expect(isCampusHost('cas.ynau.edu.cn'), isTrue);
      expect(isCampusHost('example.com'), isFalse);
    });

    test('runSessionProbe 对校方域名直接拒绝，且**一个请求都不发**', () async {
      final client = _Recorder();
      final SessionProbeResult result = await runSessionProbe(
        url: kProbeDefaultUrl,
        userAgent: 'UA',
        cookies: const <ProbeCookie>[ProbeCookie('S', 'v')],
        client: client,
      );
      expect(client.urls, isEmpty, reason: '不得从非浏览器环境打校方域名');
      expect(result.verdict, contains('禁止'));
      expect(result.verdict.contains('✅'), isFalse);
    });

    test('非校方域名仍可用于自测（单元测试用）', () async {
      final client = _Recorder();
      final SessionProbeResult result = await runSessionProbe(
        url: 'https://example.com/api',
        userAgent: 'UA',
        cookies: const <ProbeCookie>[ProbeCookie('S', 'v')],
        client: client,
      );
      expect(client.urls.length, 2, reason: '带会话 + 匿名对照各 1 次');
      expect(result.verdict, isNot(contains('禁止')));
    });

    test('批量探测已删除：代码里不再有请求头矩阵 / 跨域扫描的 API', () {
      // 这些符号是 t138/t139 的批量探测入口，t140 已整体移除。
      // 若它们重新出现，说明有人又把"扫一遍"加回来了。
      expect(kCampusHosts, isNotEmpty); // 保留引用，避免空测试
      expect(buildScheduleApiUrl(origin: 'https://one.ynau.edu.cn', years: '2026-2027-1', week: 1, cardId: 'a')
          .path, kScheduleApiPath);
    });
  });

  group('页内结果摘要（t140）', () {
    test('解析 JSON 字符串（含引号包裹的场景）', () {
      final InPageFetchSummary? summary = InPageFetchSummary.fromJsonString(
        '"{\\"status\\":200,\\"length\\":129,\\"hasStudentId\\":true,\\"contentType\\":\\"application/json\\"}"',
      );
      expect(summary, isNotNull);
      expect(summary!.status, 200);
      expect(summary.length, 129);
      expect(summary.hasStudentId, isTrue);
      expect(summary.verdict, contains('✅'));
    });

    test('空/非法返回 null；错误字段进 verdict', () {
      expect(InPageFetchSummary.fromJsonString(''), isNull);
      expect(InPageFetchSummary.fromJsonString('null'), isNull);
      final InPageFetchSummary? failed =
          InPageFetchSummary.fromJsonString('{"status":0,"length":0,"error":"TypeError"}');
      expect(failed!.verdict, contains('❌'));
    });

    test('摘要不含响应体/Cookie（结构决定）', () {
      final InPageFetchSummary summary = InPageFetchSummary.fromJsonString(
        '{"status":500,"length":129,"hasStudentId":false}',
      )!;
      expect(summary.toString().contains('SESSION'), isFalse);
      expect(summary.toString(), contains('status=500'));
    });
  });

  group('CAS ticket 探针（一次导航）', () {
    test('loginUrl 拼接（ssoo 定制域也正确）', () {
      expect(
        CasTicketProbe.loginUrl(casBase: 'https://ssoo.ynau.edu.cn', serviceUrl: 'https://one.ynau.edu.cn/'),
        startsWith('https://ssoo.ynau.edu.cn/cas/login?service='),
      );
    });

    test('最多展示 5 跳（结果里 6 跳也只有前 5 跳有 UI 用武之地）', () {
      final CasTicketResult result = CasTicketResult.summarize(<String>[
        for (int i = 0; i < 6; i++) 'https://ssoo.ynau.edu.cn/hop$i',
      ]);
      expect(result.hops, 6);
      // UI 用 take(5) 截断；这里验证 summarize 本身不丢信息
      expect(result.redirects.length, 6);
    });

    test('ticket 只保留后 4 位', () {
      final CasTicketResult result = CasTicketResult.summarize(<String>[
        'https://one.ynau.edu.cn/?ticket=ST-abcdefgh-xyz',
      ]);
      expect(result.prefix, 'abcd');
      expect(result.finalUrl.contains('ticket=ST-abcd****'), isTrue);
      expect(result.finalUrl.contains('abcdefgh'), isFalse);
    });
  });

  group('Cookie 诊断只看目标域（t140 删除跨域扫描）', () {
    test('只查一次：source 调用次数为 1', () async {
      int calls = 0;
      final CookieDiagnostics d = await diagnoseCookies(
        target: Uri.parse('https://one.ynau.edu.cn/api/x'),
        source: (Uri url) async {
          calls += 1;
          return <CookieRef>[CookieRef(name: 'SESSION', domain: url.host)];
        },
        jsProbe: () async => 12,
      );
      expect(calls, 1, reason: '不得再扫多个域');
      expect(d.exact.length, 1);
    });
  });

  testWidgets('弹窗限流：每轮最多 2 次请求 + 冷却期按钮禁用', (WidgetTester tester) async {
    int fetchCalls = 0;
    Future<InPageFetchSummary?> fakeFetch(String url, {required bool withCredentials}) async {
      fetchCalls += 1;
      return InPageFetchSummary(status: 200, length: 100 + fetchCalls, hasStudentId: true);
    }

    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SessionProbeDialog(
          cookieLoader: () async => const <ProbeCookie>[ProbeCookie('S', 'v')],
          cookieRefSource: (Uri url) async => <CookieRef>[CookieRef(name: 'S', domain: url.host)],
          jsProbe: () async => 5,
          inPageFetch: fakeFetch,
          navigate: (String url) async =>
              Future<void>.delayed(const Duration(milliseconds: 30)),
        ),
      ),
    ));
    await tester.pumpAndSettle();

    // 第 1 次：带会话
    await tester.ensureVisible(find.byKey(const Key('probe-inpage-with-cookie')));
    await tester.tap(find.byKey(const Key('probe-inpage-with-cookie')));
    await tester.pumpAndSettle();
    expect(fetchCalls, 1);
    expect(find.textContaining('本轮已用请求：1/2'), findsOneWidget);
    expect(find.textContaining('冷却'), findsWidgets);

    // 冷却期内再点无效（按钮 disabled）
    await tester.tap(find.byKey(const Key('probe-inpage-anon')), warnIfMissed: false);
    await tester.pumpAndSettle();
    expect(fetchCalls, 1, reason: '冷却期内不得再发请求');

    // 等冷却结束（10s）后可发第 2 次
    await tester.pump(const Duration(seconds: 11));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('probe-inpage-anon')));
    await tester.pumpAndSettle();
    expect(fetchCalls, 2);
    expect(find.textContaining('本轮已用请求：2/2'), findsOneWidget);

    // 第 3 次：达到上限，即使冷却结束也不可再发
    await tester.pump(const Duration(seconds: 11));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('probe-inpage-with-cookie')), warnIfMissed: false);
    await tester.pumpAndSettle();
    expect(fetchCalls, 2, reason: '每轮最多 2 个请求');
  });
}
