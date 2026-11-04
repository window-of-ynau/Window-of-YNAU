// t142：cardId 自动探测（纯本地，无网络）——解析、脱敏、填入与根因守护。

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/card_id_hints.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/campus/session_cookie_diagnostics.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';

const String _a = '0123456789abcdef0123456789abcdef';
const String _b = 'fedcba9876543210fedcba9876543210';

void main() {
  group('候选解析（t142）', () {
    test('从 window / xhr / html / localStorage 四种来源各取到候选', () {
      final List<CardIdHint> hints = parseCardIdHints(
        '[{"v":"$_a","s":"window.__STATE__.cardId"},'
        '{"v":"$_b","s":"xhr:/api/uppcard/kbsz/queryAWeekSchedule?years=…"},'
        '{"v":"$_a","s":"html"},'
        '{"v":"$_b","s":"localStorage.yunong_cache"}]',
      );
      expect(hints.length, 2, reason: '重复值要去重');
      expect(hints[0].value, _a);
      expect(hints[0].source, contains('window'));
      expect(hints[1].value, _b);
      // 去重保留**首次出现**的来源（xhr 在 localStorage 之前）
      expect(hints[1].source, contains('xhr'));
      // 脱敏：前 6 + … + 后 4
      expect(hints[0].masked, '012345…cdef');
      expect(hints[0].masked.contains(_a), isFalse);
    });

    test('evaluateJavascript 返回被引号包裹/转义的字符串也能解析', () {
      final List<CardIdHint> hints = parseCardIdHints(
        '"{\\"v\\":\\"$_a\\",\\"s\\":\\"html\\"}"'.replaceFirst('"{"', '[{"').replaceFirst('\\"}"', '\\"}]'),
      );
      // 上面构造的是单个对象，本用例只验证"引号包裹不会崩"；结构不合法时返回空。
      expect(hints, isEmpty);

      final List<CardIdHint> quoted = parseCardIdHints(
        '"[{\\"v\\":\\"$_a\\",\\"s\\":\\"html\\"}]"',
      );
      expect(quoted.length, 1);
      expect(quoted.first.value, _a);
    });

    test('非 32 位 hex、空值、非法 JSON 一律丢弃', () {
      expect(parseCardIdHints('[]'), isEmpty);
      expect(parseCardIdHints(''), isEmpty);
      expect(parseCardIdHints('not-json'), isEmpty);
      expect(
        parseCardIdHints('[{"v":"short","s":"window.x"},{"v":"${_a}zz","s":"html"}]'),
        isEmpty,
      );
    });

    test('最多 10 个候选', () {
      final StringBuffer buffer = StringBuffer('[');
      for (int i = 0; i < 15; i++) {
        final String value = i.toRadixString(16).padLeft(32, '0');
        buffer.write('${i == 0 ? '' : ','}{"v":"$value","s":"html"}');
      }
      buffer.write(']');
      expect(parseCardIdHints(buffer.toString()).length, 10);
    });

    test('找不到时的提示文案', () {
      expect(kNoCardIdHintMessage, contains('未发现 32 位 hex'));
    });
  });

  group('根因守护（t142）', () {
    test('ScheduleTemplate.isValid 只看 cardId（学期可缺，不能因此丢弃模板）', () {
      const ScheduleTemplate onlyCardId = ScheduleTemplate(cardId: _a, years: '');
      expect(onlyCardId.isValid, isTrue, reason: '旧实现要求 years 非空 → SPA 未渲染学期时丢弃模板');
      const ScheduleTemplate empty = ScheduleTemplate(cardId: '', years: '2026-2027-1');
      expect(empty.isValid, isFalse);
    });

    test('页面脚本：探测函数已注入、XHR 记录存在、兜底不再因缺学期放弃', () async {
      final String source = await File('lib/src/campus/portal_api.dart').readAsString();
      expect(source.contains('window.__yunongProbeCardIdsRaw'), isTrue,
          reason: '页面内必须有探测器');
      expect(source.contains('window.__yunongXhrLog'), isTrue, reason: '要记录已见请求供离线扫描');
      expect(source.contains('kCardIdProbeInstallScript'), isTrue);
    });

    test('探测脚本不包含网络调用（纯本地读取）', () {
      // 取安装脚本常量内容，断言没有任何 fetch/XHR/sendBeacon 之类
      const String script = kCardIdProbeInstallScript;
      expect(script.contains('fetch('), isFalse);
      expect(script.contains('XMLHttpRequest'), isFalse);
      expect(script.contains('sendBeacon'), isFalse);
      expect(script.contains('localStorage'), isTrue);
      expect(script.contains('sessionStorage'), isTrue);
    });
  });

  testWidgets('弹窗：探测出候选 → 点「用这个」填入 cardId', (WidgetTester tester) async {
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SessionProbeDialog(
          cookieLoader: () async => const <ProbeCookie>[],
          cookieRefSource: (Uri url) async => const <CookieRef>[],
          jsProbe: () async => 0,
          inPageFetch: (String url, {required bool withCredentials}) async => null,
          navigate: (String url) async {},
          probeCardIds: () async => const <CardIdHint>[
            CardIdHint(value: _a, source: 'window.__STATE__.cardId'),
            CardIdHint(value: _b, source: 'localStorage.yunong_cache'),
          ],
        ),
      ),
    ));
    await tester.pumpAndSettle();

    await tester.ensureVisible(find.byKey(const Key('probe-cardid-detect')));
    await tester.tap(find.byKey(const Key('probe-cardid-detect')));
    await tester.pumpAndSettle();

    // 两个候选，脱敏展示 + 来源
    expect(find.textContaining('012345…cdef'), findsOneWidget);
    expect(find.textContaining('window.__STATE__.cardId'), findsOneWidget);
    expect(find.textContaining('fedcba…3210'), findsOneWidget);

    // 点第一个「用这个」 → 输入框被填成完整值
    await tester.tap(find.text('用这个').first);
    await tester.pumpAndSettle();
    final TextField field = tester.widget<TextField>(find.byKey(const Key('probe-cardid')));
    expect(field.controller!.text, _a);
    expect(find.textContaining('已填入 012345…cdef'), findsOneWidget);
  });

  testWidgets('弹窗：无候选时给出明确提示', (WidgetTester tester) async {
    await tester.pumpWidget(MaterialApp(
      home: Scaffold(
        body: SessionProbeDialog(
          cookieLoader: () async => const <ProbeCookie>[],
          cookieRefSource: (Uri url) async => const <CookieRef>[],
          jsProbe: () async => 0,
          inPageFetch: (String url, {required bool withCredentials}) async => null,
          navigate: (String url) async {},
          probeCardIds: () async => const <CardIdHint>[],
        ),
      ),
    ));
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.byKey(const Key('probe-cardid-detect')));
    await tester.tap(find.byKey(const Key('probe-cardid-detect')));
    await tester.pumpAndSettle();
    expect(find.textContaining('未发现 32 位 hex'), findsOneWidget);
  });
}
