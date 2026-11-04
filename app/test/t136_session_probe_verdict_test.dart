// t136：【临时诊断】探针判读修正的测试——不联网，用注入的假客户端。
//
// 覆盖 captain 点名的四种情况：Cookie 为空 / 两次哈希相同 / 带 Cookie 不同 / 302。
// 另含安全约束：匿名对照不带 Cookie、结果绝不含 Cookie 明文。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';

class _FakeClient implements ProbeClient {
  _FakeClient(this.withCookieBody, {this.anonBody, this.statusCode = 200, this.anonStatus = 200});

  final String withCookieBody;
  final String? anonBody;
  final int statusCode;
  final int anonStatus;
  final List<Map<String, String>> headers = <Map<String, String>>[];

  @override
  Future<ProbeResponse> get({
    required Uri url,
    required Map<String, String> headers,
    required Duration timeout,
  }) async {
    this.headers.add(headers);
    final bool withCookie = headers.containsKey('Cookie');
    return ProbeResponse(
      statusCode: withCookie ? statusCode : anonStatus,
      contentType: 'text/html; charset=utf-8',
      body: withCookie ? withCookieBody : (anonBody ?? withCookieBody),
    );
  }
}

void main() {
  const List<ProbeCookie> cookies = <ProbeCookie>[
    ProbeCookie('SESSION', 'super-secret-token'),
  ];
  const String dataWithSid = '<html><title>课表</title><body>学号：2023123456</body></html>';

  test('① Cookie 为空 → 不给 ✅，只提示不能判断（即使 200 且命中学号）', () async {
    final client = _FakeClient(dataWithSid, anonBody: '<html>public</html>');
    final result = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: const <ProbeCookie>[],
      client: client,
    );
    expect(result.cookieCount, 0);
    expect(result.verdict.contains('✅'), isFalse, reason: 'Cookie 为空时绝不能给 ✅');
    expect(result.verdict, contains('未带会话'));
    expect(result.verdict, contains('不能'));
  });

  test('② 带 Cookie 与匿名哈希相同 → 判为公开页面/登录页，结果无效', () async {
    const String samePage = '<html><title>登录</title>请先登录</html>';
    final client = _FakeClient(samePage, anonBody: samePage);
    final result = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: client,
    );
    expect(result.sameAsAnonymous, isTrue);
    expect(result.verdict, contains('❌'));
    expect(result.verdict, contains('相同'));
    expect(result.verdict.contains('✅'), isFalse);
  });

  test('③ 带 Cookie 200 且内容不同、命中学号 → ✅ 且只给脱敏片段', () async {
    final client = _FakeClient(dataWithSid, anonBody: '<html><title>登录</title>请先登录</html>');
    final result = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: client,
    );
    expect(result.sameAsAnonymous, isFalse);
    expect(result.withCookie.statusCode, 200);
    expect(result.withCookie.studentIdMasked, '20***56');
    expect(result.verdict, contains('✅'));
    expect(result.verdict, contains('20***56'));
    // 脱敏：不能出现完整学号
    expect(result.toString().contains('2023123456'), isFalse);
  });

  test('④ 带 Cookie 302 → 判为会话无效（多半弹回登录页）', () async {
    final client = _FakeClient('', anonBody: '', statusCode: 302, anonStatus: 200);
    final result = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: client,
    );
    expect(result.verdict, contains('重定向'));
    expect(result.verdict.contains('✅'), isFalse);
  });

  test('200 且内容不同，但正文含「登录/统一身份认证」→ 仍判无效', () async {
    final client = _FakeClient(
      '<html><title>统一身份认证</title>请登录</html>',
      anonBody: '<html>other</html>',
    );
    final result = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: client,
    );
    expect(result.withCookie.loginHint, isTrue);
    expect(result.verdict.contains('✅'), isFalse);
    expect(result.verdict, contains('登录'));
  });

  test('安全：匿名对照不带 Cookie 头；结果不含 Cookie 明文', () async {
    final client = _FakeClient(dataWithSid, anonBody: '<html>x</html>');
    final result = await runSessionProbe(
      url: kProbeDefaultUrl,
      userAgent: 'UA',
      cookies: cookies,
      client: client,
    );
    expect(client.headers.length, 2, reason: '一次运行 = 带 Cookie + 匿名对照');
    expect(client.headers[0]['Cookie'], 'SESSION=super-secret-token');
    expect(client.headers[1].containsKey('Cookie'), isFalse, reason: '匿名请求绝不能带 Cookie');
    expect(result.toString().contains('super-secret-token'), isFalse);
    expect(result.cookieCount, 1);
  });

  test('换 UA 会真的改请求头（判断有无 UA 校验）', () async {
    final client = _FakeClient(dataWithSid, anonBody: '<html>x</html>');
    await runSessionProbe(url: kProbeDefaultUrl, userAgent: 'UA-A', cookies: cookies, client: client);
    await runSessionProbe(url: kProbeDefaultUrl, userAgent: 'UA-B', cookies: cookies, client: client);
    expect(client.headers[0]['User-Agent'], 'UA-A');
    expect(client.headers[2]['User-Agent'], 'UA-B');
  });

  test('默认 URL 是需要登录的数据接口（不是公开页面）', () {
    expect(kProbeDefaultUrl, contains('/api/uppcard/kbsz/queryAWeekSchedule'));
    expect(kProbeDefaultUrl, contains('one.ynau.edu.cn'));
  });

  test('maskStudentId：前2 + *** + 后2', () {
    expect(maskStudentId('2023123456'), '20***56');
    expect(maskStudentId('123'), '**');
  });

  testWidgets('弹窗：跑一次后同时展示两组结果与结论', (WidgetTester tester) async {
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SessionProbeDialog(
          cookieLoader: () async => cookies,
          client: _FakeClient(dataWithSid, anonBody: '<html><title>登录</title>请先登录</html>'),
        ),
      ),
    ));
    await tester.pumpAndSettle();
    expect(find.textContaining('临时诊断，验证后删除'), findsOneWidget);

    await tester.tap(find.byKey(const Key('probe-run')));
    await tester.pumpAndSettle();
    expect(find.textContaining('✅'), findsOneWidget);
    expect(find.textContaining('带 Cookie'), findsWidgets);
    expect(find.textContaining('匿名对照'), findsWidgets);
    expect(find.textContaining('20***56'), findsWidgets);
  });
}
