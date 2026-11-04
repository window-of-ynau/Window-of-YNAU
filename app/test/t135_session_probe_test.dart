// t135：【临时诊断】探针逻辑测试——不联网、不碰真实会话，用注入的假客户端验证行为与安全约束。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';

class _FakeClient implements ProbeClient {
  _FakeClient(this.response, {this.statusCode = 200});

  final String response;
  final int statusCode;
  final List<Map<String, String>> sentHeaders = <Map<String, String>>[];
  final List<String> sentUrls = <String>[];

  @override
  Future<ProbeResponse> get({
    required Uri url,
    required Map<String, String> headers,
    required Duration timeout,
  }) async {
    sentHeaders.add(headers);
    sentUrls.add(url.toString());
    return ProbeResponse(
      statusCode: statusCode,
      contentType: 'text/html; charset=utf-8',
      body: response,
    );
  }
}

void main() {
  const String cookieValue = 'SESSION=super-secret-token';
  const List<ProbeCookie> cookies = <ProbeCookie>[
    ProbeCookie('SESSION', 'super-secret-token'),
    ProbeCookie('JSESSIONID', 'abc123'),
  ];

  test('带上 Cookie 与 UA 发 GET；Cookie 出现在请求头里', () async {
    final client = _FakeClient('<html>ok</html>');
    await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA-test/1.0',
      cookies: cookies,
      client: client,
    );
    expect(client.sentUrls.single, kProbeDefaultUrl);
    expect(client.sentHeaders.single['User-Agent'], 'UA-test/1.0');
    expect(client.sentHeaders.single['Cookie'], 'SESSION=super-secret-token; JSESSIONID=abc123');
  });

  test('结果绝不含 Cookie 明文（toString 与字段都检查）', () async {
    final client = _FakeClient('<html>ok</html>');
    final result = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: client,
    );
    expect(result.toString().contains(cookieValue), isFalse, reason: 'toString 泄露了 Cookie');
    expect(result.toString().contains('abc123'), isFalse);
    expect(result.cookieCount, 2, reason: '只暴露数量');
  });

  test('200 且含 10~12 位数字 → 命中；不含 → 未命中', () async {
    final hit = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: _FakeClient('<span>学号：2023123456</span>'),
    );
    expect(hit.statusCode, 200);
    expect(hit.studentIdHit, isTrue);
    expect(hit.studentIdMatchLength, 10);
    expect(hit.verdict, contains('✅'));

    final miss = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: _FakeClient('<html>没有数字学号</html>'),
    );
    expect(miss.studentIdHit, isFalse);
    expect(miss.verdict, contains('⚠️'));
  });

  test('302 → 判读为“可能绑定浏览器特征”', () async {
    final result = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: _FakeClient('', statusCode: 302),
    );
    expect(result.verdict, contains('❌'));
    expect(result.verdict, contains('重定向'));
  });

  test('同一页面哈希相同；不同页面哈希不同（便于比对两个端点）', () async {
    final a = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: _FakeClient('<html>same</html>'),
    );
    final b = await runSessionProbe(
      url: 'https://jwxt.ynau.edu.cn/Other',
      userAgent: 'UA',
      cookies: cookies,
      client: _FakeClient('<html>same</html>'),
    );
    final c = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: _FakeClient('<html>different</html>'),
    );
    expect(a.bodyHash, b.bodyHash);
    expect(a.bodyHash, isNot(c.bodyHash));
    expect(fnv1a64Hex('<html>same</html>').length, 16);
  });

  test('换 UA 会真的改请求头（用于判断 UA 校验）', () async {
    final client = _FakeClient('<html>ok</html>');
    await runSessionProbe(url: kProbeDefaultUrl, userAgent: 'UA-A', cookies: cookies, client: client);
    await runSessionProbe(url: kProbeDefaultUrl, userAgent: 'UA-B', cookies: cookies, client: client);
    expect(client.sentHeaders[0]['User-Agent'], 'UA-A');
    expect(client.sentHeaders[1]['User-Agent'], 'UA-B');
  });

  test('非法 URL 直接报错，不发请求', () async {
    final client = _FakeClient('x');
    final result = await runSessionProbe(url: 'not-a-url', userAgent: 'UA', cookies: cookies, client: client);
    expect(client.sentUrls, isEmpty);
    expect(result.error, isNotNull);
    expect(result.verdict, contains('❌'));
  });

  testWidgets('弹窗：注入假会话与假客户端后能跑出结果', (WidgetTester tester) async {
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SessionProbeDialog(
          cookieLoader: () async => cookies,
          client: _FakeClient('<html>学号 2023123456</html>'),
        ),
      ),
    ));
    await tester.pumpAndSettle();
    expect(find.textContaining('临时诊断，验证后删除'), findsOneWidget);
    expect(find.textContaining('Cookie：2 条'), findsOneWidget);

    await tester.tap(find.byKey(const Key('probe-run')));
    await tester.pumpAndSettle();
    expect(find.textContaining('✅'), findsOneWidget);
    expect(find.textContaining('命中 10~12 位数字'), findsWidgets);
  });
}
