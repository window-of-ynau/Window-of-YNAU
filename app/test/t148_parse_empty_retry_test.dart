// t148：PARSE_EMPTY 接线（页面入口 + bridge 重取一次）与环境排查项。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_bridge.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/campus/session_cookie_diagnostics.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';
import 'package:yunong/src/core/timetable_service.dart';
import 'package:yunong/src/theme/context_menu.dart';
import 'package:yunong/src/theme/theme_controller.dart';

final String _goodHtml =
    '<table id="timetable"><tr><td>课程 节次 周次</td></tr></table>${'x' * 2400}';

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
          return _goodHtml; // 第二次（重取）是合格 HTML
        },
      );

      // 第一次上报空壳
      bridge.handle(<String, dynamic>{'type': 'timetable', 'html': '<html></html>', 'path': '/x'});
      // t150 起是"轮询等待就绪"：第一次探测会立即发生，所以这里不假设"还没取"

      // 等重取完成
      await Future<void>.delayed(const Duration(milliseconds: 30));
      expect(grabCalls, 1);
      expect(errors, isEmpty);
      expect(fetched.length, 1);
      expect(fetched.single.html, _goodHtml);
      expect(bridge.lastHtmlDiagnostics, contains('（第 2 次）'));
      expect(bridge.lastHtmlDiagnostics, contains('是否含课表关键字=true'));
    });

    test('重取仍是空壳 → 才报 HTML_CHANGED，且诊断含两次结果', () async {
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
      // t150：轮询结束后报 HTML_CHANGED（不是登录页），消息里带每次诊断
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

    test('页面侧抓取入口：幂等安装 + 不含网络调用', () {
      expect(kGrabHtmlInstallScript.contains('window.__yunongGrabHtml'), isTrue);
      expect(kGrabHtmlInstallScript.contains('if (window.__yunongGrabHtml) { return; }'), isTrue,
          reason: '必须幂等，重复注入不覆盖');
      expect(kGrabHtmlInstallScript.contains('fetch('), isFalse);
      expect(kGrabHtmlInstallScript.contains('XMLHttpRequest'), isFalse);
      expect(kGrabHtmlScript, contains('__yunongGrabHtml'));
    });
  });

  group('环境排查项（t148）', () {
    Widget dialogUnder({required bool withSolidSurface}) {
      final ThemeData base = ThemeController(
        store: MemorySettingsStore(),
        initialTheme: AppThemeId.deepGlass,
      ).theme(Brightness.dark);
      final Widget child = MaterialApp(
        home: Scaffold(
          body: SessionProbeDialog(
            cookieLoader: () async => const <ProbeCookie>[],
            cookieRefSource: (Uri url) async => const <CookieRef>[],
            jsProbe: () async => 0,
            inPageFetch: (String url, {required bool withCredentials}) async => null,
            navigate: (String url) async {},
            evaluateScript: (String source) async => '[]',
          ),
        ),
      );
      if (!withSolidSurface) {
        return Theme(data: base, child: child);
      }
      return Theme(
        key: const Key('app-solid-surface'),
        data: solidSurfaceTheme(base),
        child: child,
      );
    }

    testWidgets('builder 生效时：显示 alpha=1.00 且提示包裹层存在', (WidgetTester tester) async {
      await tester.pumpWidget(dialogUnder(withSolidSurface: true));
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.byKey(const Key('probe-env-alpha')));
      expect(find.textContaining('surface.alpha=1.00'), findsOneWidget);
      expect(find.textContaining('builder 生效'), findsOneWidget);
    });

    testWidgets('没有 solid 包裹层时：明确提示 builder 可能未生效', (WidgetTester tester) async {
      await tester.pumpWidget(dialogUnder(withSolidSurface: false));
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.byKey(const Key('probe-env-builder')));
      expect(find.textContaining('builder 可能未生效'), findsOneWidget);
    });
  });
}
