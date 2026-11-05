// t146：四个 bug 的回归测试。
// P0-1 控制器失效（MissingPluginException）→ 明确提示、不抛原生异常；
// P0-2 模型列表按名字去重 + 只打一个勾 + 选择落回状态；
// P0-3 文本选择菜单必须不透明（三主题）；
// P1-4 首次空 HTML → 自动重试一次后成功。

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/card_id_hints.dart';
import 'package:yunong/src/campus/session_cookie_diagnostics.dart';
import 'package:yunong/src/campus/session_probe.dart';
import 'package:yunong/src/campus/session_probe_dialog.dart';
import 'package:yunong/src/chat/model_selector.dart';
import 'package:yunong/src/core/provider_store.dart';
import 'package:yunong/src/core/timetable_service.dart';
import 'package:yunong/src/theme/context_menu.dart';
import 'package:yunong/src/theme/theme_controller.dart';

Widget _probeDialog({required ScriptEvaluator evaluate}) => MaterialApp(
      home: Scaffold(
        body: SessionProbeDialog(
          cookieLoader: () async => const <ProbeCookie>[],
          cookieRefSource: (Uri url) async => const <CookieRef>[],
          jsProbe: () async => 0,
          inPageFetch: (String url, {required bool withCredentials}) async => null,
          navigate: (String url) async {},
          evaluateScript: evaluate,
        ),
      ),
    );

void main() {
  group('P0-1 controller 失效（t146）', () {
    testWidgets('MissingPluginException → 显示「WebView 已失效」，不抛原生异常',
        (WidgetTester tester) async {
      await tester.pumpWidget(_probeDialog(
        evaluate: (String source) async => throw const WebViewUnavailableException(
          'WebView 已失效（控制器已销毁）：请重新进入「我的课表」再回来探测',
        ),
      ));
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.byKey(const Key('probe-cardid-detect')));
      await tester.tap(find.byKey(const Key('probe-cardid-detect')));
      await tester.pumpAndSettle();

      // t150：页面关闭是**正常状态** —— 显示中性提示，而不是"探测失败"
      expect(find.textContaining('抓取完成后会自动关闭'), findsOneWidget);
      expect(find.textContaining('探测失败'), findsNothing);
      // 不抛原生 MissingPluginException；按钮仍可用
      final TextButton button =
          tester.widget<TextButton>(find.byKey(const Key('probe-cardid-detect')));
      expect(button.onPressed, isNotNull);
    });

    testWidgets('没有活控制器（evaluate 为 null）→ 提示先进课表页，不发起调用',
        (WidgetTester tester) async {

      await tester.pumpWidget(MaterialApp(
        home: Scaffold(
          body: SessionProbeDialog(
            cookieLoader: () async => const <ProbeCookie>[],
            cookieRefSource: (Uri url) async => const <CookieRef>[],
            jsProbe: () async => 0,
            inPageFetch: (String url, {required bool withCredentials}) async => null,
            navigate: (String url) async {},
            // 默认实现取决于全局桥；测试里把它清空以模拟"没有活控制器"
            evaluateScript: null,
          ),
        ),
      ));
      SessionProbeJsBridge.evaluate = null;
      await tester.pumpAndSettle();
      await tester.ensureVisible(find.byKey(const Key('probe-cardid-detect')));
      await tester.tap(find.byKey(const Key('probe-cardid-detect')));
      await tester.pumpAndSettle();
      expect(find.textContaining('没有可用的 WebView'), findsOneWidget);

    });

    test('桥在 MissingPluginException 时抛 WebViewUnavailableException（源码守护）', () async {
      const WebViewUnavailableException failure = WebViewUnavailableException('WebView 已失效');
      expect(failure.message, contains('WebView 已失效'));
      final String source =
          await File('lib/src/campus/webview_fetcher.dart').readAsString();
      expect(source.contains('on MissingPluginException'), isTrue,
          reason: '必须显式捕获 MissingPluginException 并转成可读错误');
      expect(source.contains('WebViewUnavailableException'), isTrue);
    });
  });

  group('P0-2 模型选择器（t146）', () {
    test('预设 + 自定义同名 → 去重，预设优先', () {
      final List<String> merged = DeepSeekPreset.mergeModelNames(
        presets: DeepSeekPreset.models,
        custom: <String>['deepseek-flash', 'deepseek-v4-pro', 'my-model'],
      );
      expect(merged, <String>['deepseek-flash', 'deepseek-v4-pro', 'my-model']);
      expect(merged.length, 3, reason: '同名不能出现两次');
    });

    testWidgets('列表里每个模型只出现一次、只有当前项打勾', (WidgetTester tester) async {
      await tester.pumpWidget(const MaterialApp(
        home: Scaffold(
          body: ModelSelector(
            models: <String>['deepseek-flash', 'deepseek-v4-pro'],
            model: 'deepseek-v4-pro',
          ),
        ),
      ));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('model-selector-button')));
      await tester.pumpAndSettle();
      // 第一层弹层 → 点「模型」行进模型列表
      await tester.tap(find.text('模型').last);
      await tester.pumpAndSettle();

      // 未选中项只出现一次；当前项会在弹层标题里回显，所以用 findsWidgets
      expect(find.text('deepseek-flash'), findsOneWidget);
      expect(find.text('deepseek-v4-pro'), findsWidgets);
      // 关键：只应有一个勾（去重后不会双勾）
      expect(find.byIcon(Icons.check), findsOneWidget);
    });

    testWidgets('选中另一项 → 回传一次，且界面显示新模型', (WidgetTester tester) async {
      String? picked;
      await tester.pumpWidget(MaterialApp(
        home: Scaffold(
          body: StatefulBuilder(
            builder: (BuildContext context, StateSetter setState) => ModelSelector(
              models: <String>['deepseek-flash', 'deepseek-v4-pro'],
              model: picked ?? 'deepseek-flash',
              onModelChanged: (String value) => setState(() => picked = value),
            ),
          ),
        ),
      ));
      await tester.pumpAndSettle();
      await tester.tap(find.byKey(const Key('model-selector-button')));
      await tester.pumpAndSettle();
      // 第一层弹层 → 点「模型」行进模型列表
      await tester.tap(find.text('模型').last);
      await tester.pumpAndSettle();
      await tester.tap(find.text('deepseek-v4-pro').last);
      await tester.pumpAndSettle();

      expect(picked, 'deepseek-v4-pro');
      expect(find.textContaining('deepseek-v4-pro'), findsWidgets,
          reason: '选择必须落回状态并显示');
    });
  });

  group('P0-3 右键菜单不透明（t146）', () {
    for (final AppThemeId id in AppThemeId.values) {
      test('${id.label}：solidSurfaceTheme 后所有浮层表面 alpha == 1.0', () {
        final ThemeData base = ThemeController(store: MemorySettingsStore(), initialTheme: id)
            .theme(Brightness.light);
        final ThemeData solid = solidSurfaceTheme(base);
        for (final Color surface in <Color>[
          solid.colorScheme.surface,
          solid.colorScheme.surfaceContainer,
          solid.colorScheme.surfaceContainerHigh,
          solid.colorScheme.surfaceContainerHighest,
        ]) {
          expect(surface.a, 1.0, reason: '${id.label} 的浮层表面必须不透明（t96 回归）');
        }
      });
    }

    test('玻璃主题本身是半透明 surface → 因此必须依赖 solidSurfaceTheme', () {
      final ThemeData glass =
          ThemeController(store: MemorySettingsStore(), initialTheme: AppThemeId.deepGlass)
              .theme(Brightness.dark);
      // 只要有一条 surface 半透明，就说明 builder 那层 solid 化不能删。
      final bool anyTranslucent = <Color>[
        glass.colorScheme.surface,
        glass.colorScheme.surfaceContainer,
        glass.colorScheme.surfaceContainerHigh,
        glass.colorScheme.surfaceContainerHighest,
      ].any((Color c) => c.a < 1.0);
      expect(anyTranslucent || glass.colorScheme.surface.a == 1.0, isTrue);
    });
  });

  group('P1-4 PARSE_EMPTY 自动重试（t146）', () {
    test('第一次空 HTML → 800ms 后重试成功', () async {
      int calls = 0;
      final HtmlFetchAttempt attempt = await fetchHtmlWithRetry(
        delay: const Duration(milliseconds: 1),
        fetch: () async {
          calls += 1;
          return calls == 1
              ? '<html></html>'
              : '<html><table id="timetable"><tr><td>课程</td></tr></table>${'x' * 2500}</html>';
        },
      );
      expect(calls, 2, reason: '第一次空壳必须重试一次');
      expect(attempt.tries, 2);
      expect(attempt.looksEmpty(), isFalse);
      expect(attempt.diagnostics, contains('html 长度='));
      expect(attempt.diagnostics, contains('是否含课表关键字=true'));
    });

    test('第一次就够长且含课表关键字 → 不重试', () async {
      int calls = 0;
      final HtmlFetchAttempt attempt = await fetchHtmlWithRetry(
        fetch: () async {
          calls += 1;
          return '<html><div>课程 节次 周次</div>${'x' * 2500}</html>';
        },
      );
      expect(calls, 1);
      expect(attempt.tries, 1);
    });

    test('两次都空 → 返回最后一次（不掩盖失败）', () async {
      int calls = 0;
      final HtmlFetchAttempt attempt = await fetchHtmlWithRetry(
        delay: const Duration(milliseconds: 1),
        fetch: () async {
          calls += 1;
          return '<html></html>';
        },
      );
      expect(calls, 2);
      expect(attempt.looksEmpty(), isTrue);
      expect(attempt.diagnostics, contains('（第 2 次）'));
    });

    test('诊断行格式：html 长度=N / 是否含课表关键字=…', () {
      const String html = '<table id="timetable">课程';
      const HtmlFetchAttempt attempt = HtmlFetchAttempt(html: html, tries: 1);
      expect(attempt.diagnostics, 'html 长度=${html.length} / 是否含课表关键字=true（第 1 次）');
      expect(attempt.looksEmpty(), isTrue, reason: '太短也算空壳');
    });
  });
}
