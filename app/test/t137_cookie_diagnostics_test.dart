// t137：【临时诊断】Cookie 恒为 0 的三步排查——纯逻辑测试（不联网、不碰真实 WebView）。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/session_cookie_diagnostics.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';

void main() {
  final Uri target = Uri.parse('https://one.ynau.edu.cn/api/x');

  CookieRefSource source(Map<String, List<String>> byHost) {
    return (Uri url) async {
      final List<String> names = byHost[url.host] ?? const <String>[];
      return names.map((String name) => CookieRef(name: name, domain: url.host)).toList();
    };
  }

  test('精确 URL 有 Cookie → 诊断说明“之前是域不匹配”', () async {
    final d = await diagnoseCookies(
      target: target,
      source: source(<String, List<String>>{
        'one.ynau.edu.cn': <String>['SESSION'],
        'jwxt.ynau.edu.cn': <String>['JSESSIONID'],
      }),
      jsProbe: () async => 0,
    );
    expect(d.exact.length, 1);
    expect(d.diagnosis, contains('✅'));
    expect(d.diagnosis, contains('域'));
    expect(d.toString().contains('SESSION'), isFalse, reason: 'toString 只应有数量');
  });

  test('精确 URL 0 条 但 JS 有内容 → 强烈指向 HttpOnly', () async {
    final d = await diagnoseCookies(
      target: target,
      source: source(<String, List<String>>{}),
      jsProbe: () async => 42,
    );
    expect(d.exact, isEmpty);
    expect(d.jsCookieLength, 42);
    expect(d.diagnosis, contains('HttpOnly'));
  });

  test('精确 URL 0 条、JS 也为 0 → 无会话/时序', () async {
    final d = await diagnoseCookies(
      target: target,
      source: source(<String, List<String>>{}),
      jsProbe: () async => 0,
    );
    expect(d.diagnosis, contains('无会话'));
  });

  test('目标域 0 条但其它域有 → 域不匹配', () async {
    final d = await diagnoseCookies(
      target: target,
      source: source(<String, List<String>>{
        'jwxt.ynau.edu.cn': <String>['JSESSIONID', 'route'],
      }),
      jsProbe: () async => 0,
    );
    expect(d.exact, isEmpty);
    expect(d.diagnosis, contains('域不匹配'));
  });

  test('JS 拿不到（null）→ 提示 WebView 未就绪', () async {
    final d = await diagnoseCookies(
      target: target,
      source: source(<String, List<String>>{}),
      jsProbe: () async => null,
    );
    expect(d.jsCookieLength, isNull);
    expect(d.diagnosis, contains('WebView'));
  });

  test('跨域扫描包含四个校方域；每域只给数量与名字', () async {
    final d = await diagnoseCookies(
      target: target,
      source: source(<String, List<String>>{'cas.ynau.edu.cn': <String>['CASTGC']}),
      jsProbe: () async => 0,
    );
    final List<String> hosts = d.scans.map((DomainScan s) => s.domain).toList();
    expect(hosts, containsAll(<String>['one.ynau.edu.cn', 'jwxt.ynau.edu.cn', 'cas.ynau.edu.cn', 'ynau.edu.cn']));
    final DomainScan cas = d.scans.firstWhere((DomainScan s) => s.domain == 'cas.ynau.edu.cn');
    expect(cas.count, 1);
    expect(cas.cookies.single.name, 'CASTGC');
  });

  test('parseJsCookieLength 处理带引号与空值', () {
    expect(parseJsCookieLength('42'), 42);
    expect(parseJsCookieLength('"42"'), 42);
    expect(parseJsCookieLength(''), isNull);
    expect(parseJsCookieLength('null'), isNull);
  });

  test('describeCookieRefs 只输出 name@domain', () {
    expect(
      describeCookieRefs(<CookieRef>[const CookieRef(name: 'SESSION', domain: 'one.ynau.edu.cn')]),
      'SESSION@one.ynau.edu.cn',
    );
  });

  testWidgets('弹窗里能跑三步诊断并展示判定', (WidgetTester tester) async {
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SessionProbeDialog(
          cookieLoader: () async => const <ProbeCookie>[],
          cookieRefSource: source(<String, List<String>>{}),
          jsProbe: () async => 42,
        ),
      ),
    ));
    await tester.pumpAndSettle();

    // t138：弹窗里新增了「请求头对照 / CAS」两段，诊断按钮可能被推到可视区外
    await tester.ensureVisible(find.byKey(const Key('probe-diagnose')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('probe-diagnose')));
    await tester.pumpAndSettle();
    // 结论区（顶部）+ 明细区都会出现「HttpOnly」，所以用 findsWidgets
    expect(find.textContaining('HttpOnly'), findsWidgets);
    expect(find.textContaining('document.cookie 长度'), findsWidgets);
    expect(find.textContaining('jwxt.ynau.edu.cn：0 条'), findsWidgets);
  });
}
