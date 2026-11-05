// t143：cardId 探测改用 evaluateJavascript 直接返回值——null / 非法 JSON / 超时都要能被看见，
// 且**超时后按钮必须恢复可用**（这正是本次 "按钮卡死" 的回归点）。

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/card_id_hints.dart';
import 'package:yunong/src/campus/session_cookie_diagnostics.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';

const String _a = '0123456789abcdef0123456789abcdef';
const String _b = 'fedcba9876543210fedcba9876543210';

Widget _dialog({required ScriptEvaluator evaluate, Duration timeout = const Duration(seconds: 5)}) {
  return MaterialApp(
    home: Scaffold(
      body: SessionProbeDialog(
        cookieLoader: () async => const <ProbeCookie>[],
        cookieRefSource: (Uri url) async => const <CookieRef>[],
        jsProbe: () async => 0,
        inPageFetch: (String url, {required bool withCredentials}) async => null,
        navigate: (String url) async {},
        evaluateScript: evaluate,
        cardIdProbeTimeout: timeout,
      ),
    ),
  );
}

Future<void> _tapDetect(WidgetTester tester) async {
  await tester.ensureVisible(find.byKey(const Key('probe-cardid-detect')));
  await tester.pumpAndSettle();
  await tester.tap(find.byKey(const Key('probe-cardid-detect')));
  await tester.pumpAndSettle();
}

void main() {
  group('直接返回值解析（t143）', () {
    test('返回两个候选 → 正确解析出值与来源', () {
      final CardIdProbeOutcome outcome = parseCardIdProbeReturn(
        '[{"v":"$_a","s":"window.__STATE__.cardId"},{"v":"$_b","s":"localStorage.cache"}]',
      );
      expect(outcome.ok, isTrue);
      expect(outcome.hints.length, 2);
      expect(outcome.hints[0].value, _a);
      expect(outcome.hints[1].source, contains('localStorage'));
    });

    test('返回 null → 明确报"返回 null"', () {
      expect(parseCardIdProbeReturn(null).error, contains('返回 null'));
    });

    test('返回非字符串（比如对象/Promise）→ 明确报类型', () {
      final CardIdProbeOutcome outcome = parseCardIdProbeReturn(<String, String>{'a': 'b'});
      expect(outcome.error, contains('不是字符串'));
      expect(outcome.error, contains('Map'));
    });

    test('非法 JSON → 明确报解析错误', () {
      expect(parseCardIdProbeReturn('not-json').error, contains('不是合法 JSON'));
    });

    test('不在课表页 → 提示先进入「我的课表」', () {
      final CardIdProbeOutcome outcome =
          parseCardIdProbeReturn('{"error":"$kNotOnSchedulePageError"}');
      expect(outcome.error, contains('不在课表页'));
    });

    test('探测表达式自带课表页判断，且不含网络调用', () {
      expect(kCardIdProbeExpression.contains(kNotOnSchedulePageError), isTrue);
      expect(kCardIdProbeExpression.contains('fetch('), isFalse);
      expect(kCardIdProbeExpression.contains('XMLHttpRequest'), isFalse);
      expect(kCardIdProbeExpression.contains('sendBeacon'), isFalse);
    });

    test('runCardIdProbe：5s 超时返回可读错误（不抛）', () async {
      final CardIdProbeOutcome outcome = await runCardIdProbe(
        timeout: const Duration(milliseconds: 50),
        evaluate: (String source) => Completer<Object?>().future, // 永不返回
      );
      expect(outcome.error, contains('探测超时'));
    });

    test('runCardIdProbe：evaluate 抛异常也被收敛成错误', () async {
      final CardIdProbeOutcome outcome = await runCardIdProbe(
        evaluate: (String source) async => throw StateError('boom'),
      );
      expect(outcome.error, contains('探测失败'));
    });
  });

  group('弹窗交互（t143）', () {
    testWidgets('成功：显示候选，点「用这个」填入完整值', (WidgetTester tester) async {
      await tester.pumpWidget(_dialog(
        evaluate: (String source) async =>
            '[{"v":"$_a","s":"window.__STATE__.cardId"},{"v":"$_b","s":"localStorage.cache"}]',
      ));
      await tester.pumpAndSettle();

      // 入口提示：需在课表页执行
      expect(find.textContaining('需在课表页执行'), findsOneWidget);

      await _tapDetect(tester);
      expect(find.textContaining('012345…cdef'), findsOneWidget);
      expect(find.textContaining('fedcba…3210'), findsOneWidget);

      await tester.tap(find.text('用这个').first);
      await tester.pumpAndSettle();
      // t146：输入框已删除 → 断言状态行显示脱敏值
      expect(
        tester.widget<Text>(find.byKey(const Key('probe-cardid-status'))).data,
        contains('012345…cdef'),
      );
    });

    testWidgets('返回 null：显示错误，按钮恢复可用', (WidgetTester tester) async {
      await tester.pumpWidget(_dialog(evaluate: (String source) async => null));
      await tester.pumpAndSettle();
      await _tapDetect(tester);

      expect(find.byKey(const Key('probe-cardid-message')), findsOneWidget);
      expect(find.textContaining('返回 null'), findsOneWidget);
      // 按钮恢复（文案回到「自动探测 cardId」且可点）
      expect(find.text('自动探测 cardId'), findsOneWidget);
      await tester.tap(find.byKey(const Key('probe-cardid-detect')));
      await tester.pumpAndSettle();
    });

    testWidgets('非法 JSON：显示解析错误', (WidgetTester tester) async {
      await tester.pumpWidget(_dialog(evaluate: (String source) async => 'oops-not-json'));
      await tester.pumpAndSettle();
      await _tapDetect(tester);
      expect(find.textContaining('不是合法 JSON'), findsOneWidget);
    });

    testWidgets('**超时：显示超时错误且按钮恢复可用**（本次 bug 的回归点）', (WidgetTester tester) async {
      await tester.pumpWidget(_dialog(
        timeout: const Duration(milliseconds: 200),
        evaluate: (String source) => Completer<Object?>().future, // 永不返回
      ));
      await tester.pumpAndSettle();

      await tester.tap(find.byKey(const Key('probe-cardid-detect')));
      await tester.pump(); // 进入探测中
      expect(find.text('探测中…'), findsOneWidget);

      // 推进到超时之后
      await tester.pump(const Duration(milliseconds: 250));
      await tester.pumpAndSettle();

      expect(find.textContaining('探测超时'), findsOneWidget);
      // 关键断言：按钮不再灰、文案恢复，且 onPressed 重新可用
      expect(find.text('自动探测 cardId'), findsOneWidget);
      final TextButton button =
          tester.widget<TextButton>(find.byKey(const Key('probe-cardid-detect')));
      expect(button.onPressed, isNotNull, reason: '超时后按钮必须恢复可用（本次 bug 的回归点）');
    });
  });
}
