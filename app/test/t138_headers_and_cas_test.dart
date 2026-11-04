// t138：412 反爬判读的对照（三组请求头）、CAS ticket 探针判读、Cookie 取 0 的根因修复回归。

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/session_cookie_diagnostics.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';

class _StatusClient implements ProbeClient {
  _StatusClient(this.statusForAgent);

  /// 按 UA 决定状态码（模拟"换 UA 就被拦"）。
  final int Function(String userAgent) statusForAgent;
  final List<Map<String, String>> headers = <Map<String, String>>[];

  @override
  Future<ProbeResponse> get({
    required Uri url,
    required Map<String, String> headers,
    required Duration timeout,
  }) async {
    this.headers.add(headers);
    final int status = statusForAgent(headers['User-Agent'] ?? '');
    return ProbeResponse(statusCode: status, contentType: 'text/html', body: 'x');
  }
}

void main() {
  const List<ProbeCookie> cookies = <ProbeCookie>[ProbeCookie('SESSION', 'secret')];

  group('三组请求头对照（t138）', () {
    test('裸 412 → 加浏览器头 200：判为"只差请求头"', () async {
      final client = _StatusClient((String ua) => ua == 'Dart/HttpClient' ? 412 : 200);
      final trials = await runHeaderMatrix(
        url: 'https://one.ynau.edu.cn/api/x',
        userAgent: kProbeDefaultUserAgent,
        cookies: cookies,
        client: client,
      );
      expect(trials.length, 3);
      expect(trials[0].run.statusCode, 412);
      expect(trials[1].run.statusCode, 200);
      expect(headerMatrixVerdict(trials), contains('只差请求头'));
      // 浏览器头必须真的带上
      expect(client.headers[1]['Sec-Fetch-Mode'], 'cors');
      expect(client.headers[1]['Accept-Language'], contains('zh-CN'));
      expect(client.headers[1]['Cookie'], isNotNull);
      // 裸请求档不带那些头
      expect(client.headers[0].containsKey('Sec-Fetch-Mode'), isFalse);
    });

    test('裸 412 → 加头 302：判为"反爬过了但仍需登录"', () async {
      final client = _StatusClient((String ua) => ua == 'Dart/HttpClient' ? 412 : 302);
      final trials = await runHeaderMatrix(
        url: 'https://one.ynau.edu.cn/api/x',
        userAgent: kProbeDefaultUserAgent,
        cookies: cookies,
        client: client,
      );
      expect(headerMatrixVerdict(trials), contains('仍需登录'));
    });

    test('三组都 412：判为反爬不止校验请求头', () async {
      final client = _StatusClient((_) => 412);
      final trials = await runHeaderMatrix(
        url: 'https://one.ynau.edu.cn/api/x',
        userAgent: kProbeDefaultUserAgent,
        cookies: cookies,
        client: client,
      );
      expect(headerMatrixVerdict(trials), contains('仍是 412'));
    });
  });

  group('CAS ticket 探针（t138 方案 E）', () {
    test('重定向链出现 ticket=ST- → 判定可用，且只保留 4 位前缀', () {
      final result = CasTicketResult.summarize(<String>[
        'https://cas.ynau.edu.cn/cas/login?service=https%3A%2F%2Fexample.com%2F',
        'https://cas.ynau.edu.cn/cas/login?username=2023123456',
        'https://example.com/?ticket=ST-1234567890-abcdef-cas',
      ]);
      expect(result.ticketFound, isTrue);
      expect(result.verdict, contains('✅'));
      // 完整 ticket 不得出现在结果里
      expect(result.redirects.join().contains('ST-1234567890'), isFalse);
      expect(result.finalUrl.contains('ticket=ST-1234****'), isTrue);
    });

    test('没有 ticket → 判定失败并给出跳数与终点域', () {
      final result = CasTicketResult.summarize(<String>[
        'https://cas.ynau.edu.cn/cas/login?service=x',
        'https://cas.ynau.edu.cn/cas/login',
      ]);
      expect(result.ticketFound, isFalse);
      expect(result.verdict, contains('未拿到 ticket'));
      expect(result.finalHost, 'cas.ynau.edu.cn');
    });

    test('零跳 → 提示 WebView 未就绪/需人工登录', () {
      expect(CasTicketResult.summarize(const <String>[]).verdict, contains('没有观察到任何重定向'));
    });

    test('loginUrl 拼接正确（避免 /cas/cas）', () {
      expect(
        CasTicketProbe.loginUrl(casBase: 'https://cas.ynau.edu.cn', serviceUrl: 'https://example.com/'),
        startsWith('https://cas.ynau.edu.cn/cas/login?service='),
      );
      expect(
        CasTicketProbe.loginUrl(casBase: 'https://cas.ynau.edu.cn/cas', serviceUrl: 'https://example.com/'),
        startsWith('https://cas.ynau.edu.cn/cas/login?service='),
      );
    });

    test('record 只在 recording 时写入，且最多 30 跳', () {
      CasTicketProbe.stop();
      CasTicketProbe.redirects.clear();
      CasTicketProbe.record('https://a');
      expect(CasTicketProbe.redirects, isEmpty);

      CasTicketProbe.start();
      for (int i = 0; i < 40; i++) {
        CasTicketProbe.record('https://hop$i');
      }
      expect(CasTicketProbe.redirects.length, 30);
      CasTicketProbe.stop();
      CasTicketProbe.redirects.clear();
    });

    test('shortUrl 截断到 120 字符', () {
      final String long = 'https://one.ynau.edu.cn/${'a' * 200}';
      expect(shortUrl(long).length, 121); // 120 + 省略号
    });
  });

  group('Cookie 恒为 0 的根因（t138）', () {
    test('诊断结论：CookieManager 0 条且 JS>0 时明确写 HttpOnly 可能性', () async {
      final d = await diagnoseCookies(
        target: Uri.parse('https://one.ynau.edu.cn/api/x'),
        source: (Uri url) async => const <CookieRef>[],
        jsProbe: () async => 128,
      );
      expect(d.diagnosis, contains('HttpOnly'));
    });

    test('环境修复的守护：loadCampusCookieRefs 必须传 webViewEnvironment（源码断言）', () async {
      // 结构性守护：本文件对应的实现里必须出现 `CookieManager.instance(webViewEnvironment:`
      // （缺省环境会让插件静默返回空列表 —— 这是 t138 定位到的根因）
      final String source = await _readSource();
      expect(source.contains('CookieManager.instance(webViewEnvironment:'), isTrue,
          reason: '必须传 WebViewEnvironment，否则 Cookie 恒为 0');
    });
  });

  testWidgets('弹窗：三组对照与 CAS 按钮都在，且顶部有结论区', (WidgetTester tester) async {
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SessionProbeDialog(
          cookieLoader: () async => const <ProbeCookie>[],
          client: _StatusClient((String ua) => ua == 'Dart/HttpClient' ? 412 : 412),
          cookieRefSource: (Uri url) async => const <CookieRef>[],
          jsProbe: () async => 0,
          navigate: (String url) async {},
        ),
      ),
    ));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('probe-headers')), findsOneWidget);
    expect(find.byKey(const Key('probe-cas')), findsOneWidget);
    expect(find.byKey(const Key('probe-cas-base')), findsOneWidget);
    expect(find.byKey(const Key('probe-service')), findsOneWidget);
    expect(find.byKey(const Key('probe-diagnose')), findsOneWidget);

    await tester.tap(find.byKey(const Key('probe-headers')));
    await tester.pumpAndSettle();
    // 结论区应顶到最上面（owner 反馈看不到输出）
    expect(find.byKey(const Key('probe-verdict-box')), findsOneWidget);
    expect(find.textContaining('仍是 412'), findsOneWidget);
  });
}

Future<String> _readSource() async {
  // Flutter test 跑在 VM 上，直接读仓库里的源码文件即可。
  final File file = File('lib/src/campus/session_probe_dialog.dart');
  return file.readAsString();
}
