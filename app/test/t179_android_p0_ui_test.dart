// t179：release 必须自带 INTERNET 权限；Android 数据根必须是应用私有目录（不再 //yunong）；三处 UI 收尾。

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/core/app_paths.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

Future<void> _pumpApp(WidgetTester tester, Size size) async {
  setTestWindow(tester, size.width, size.height);
  await tester.pumpWidget(YunongApp(
    themeController: ThemeController(store: MemorySettingsStore()),
  ));
  await tester.pumpAndSettle();
}

void main() {
  group('P0-1 Android release 联网权限（t179）', () {
    test('src/main/AndroidManifest.xml 必须声明 INTERNET 与 ACCESS_NETWORK_STATE', () {
      final String manifest =
          File('android/app/src/main/AndroidManifest.xml').readAsStringSync();
      expect(manifest, contains('android.permission.INTERNET'),
          reason: 'release 只读 src/main；缺 INTERNET 会表现为 WebView 一片空白');
      expect(manifest, contains('android.permission.ACCESS_NETWORK_STATE'));
      // 防止以后又被模板覆盖：权限必须出现在 <application 之前
      final int permAt = manifest.indexOf('android.permission.INTERNET');
      final int appAt = manifest.indexOf('<application');
      expect(permAt, greaterThan(0));
      expect(permAt, lessThan(appAt));
    });
  });

  group('P0-2 Android 数据根（t179）', () {
    tearDown(() => AppPaths.debugSetPlatformBase(null));

    test('手机：应用私有目录 → <base>/yunong，绝不出现 //', () {
      const String base = '/data/user/0/cn.ynau.yunong/files';
      AppPaths.debugSetPlatformBase(base);
      final String dir = AppPaths.dataDirUnder(base, debug: false);
      expect(dir.replaceAll(r'\', '/'), '$base/yunong');
      expect(dir, isNot(startsWith('//')));
      expect(dir.contains('//'), isFalse);
    });

    test('normalizeBase：重复分隔符、尾部分隔符、空串都归一化', () {
      final String sep = Platform.pathSeparator;
      final String doubled = '$sep$sep';
      expect(AppPaths.normalizeBase(doubled), sep, reason: '折叠成单个分隔符');
      expect(AppPaths.normalizeBase('${sep}data${sep}user$sep'), isNot(endsWith(sep)));
      expect(AppPaths.normalizeBase(''), isEmpty);
      // 关键回归：任何以 // 开头的输入都不该原样返回
      expect(AppPaths.normalizeBase('${doubled}yunong'), isNot(startsWith(doubled)));
    });

    test('mobileRootProblem：手机缺平台目录 → 明确错误（不再静默回落 //yunong）', () {
      expect(AppPaths.mobileRootProblem(isMobile: true, platformBase: null), isNotNull);
      expect(AppPaths.mobileRootProblem(isMobile: true, platformBase: '  '), isNotNull);
      expect(AppPaths.mobileRootProblem(isMobile: true, platformBase: '/data/user/0/x/files'), isNull);
      expect(AppPaths.mobileRootProblem(isMobile: false, platformBase: null), isNull);
    });

    test('baseDirFrom：手机分支不吃 HOME（Android 的 HOME=/ 会拼出 //yunong）', () {
      expect(
        AppPaths.baseDirFrom(<String, String>{'HOME': '/'}, isWindows: false, isMobile: true),
        isEmpty,
      );
      expect(
        AppPaths.baseDirFrom(<String, String>{'HOME': '/home/u'}, isWindows: false, isMobile: false),
        contains('home'),
      );
    });
  });

  group('P1 UI 收尾（t179）', () {
    testWidgets('(a) 手机端内容与 rail 都在 SafeArea 内（避让状态栏/手势条）',
        (WidgetTester tester) async {
      await _pumpApp(tester, const Size(360, 800));
      expect(find.byType(SafeArea), findsWidgets);
    });

    testWidgets('(b) PC 状态行更靠近顶部（顶部内边距 ≤ 4）', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      final Finder status = find.byKey(const Key('chat-top-bar'));
      if (status.evaluate().isNotEmpty) {
        expect(tester.getRect(status.first).top, lessThanOrEqualTo(4));
      }
    });

    testWidgets('(c) composer 单层：高度 52、圆角、只 1px 边框、输入框无自身边框',
        (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      final Finder composer = find.byKey(const Key('composer'));
      final Rect rect = tester.getRect(composer);
      final Container box = tester.widget<Container>(composer);
      // 52 是容器自身高度；rect 还包含上下外边距（8 + 12）
      expect(box.constraints?.minHeight, closeTo(52, 0.01), reason: '单层胶囊高 52');
      expect(rect.height, closeTo(52 + 8 + 12, 1), reason: '含外边距总高 72');
      final BoxDecoration? deco = box.decoration as BoxDecoration?;
      expect(deco, isNotNull);
      expect(deco!.borderRadius, isNotNull);
      final Border border = deco.border! as Border;
      expect(border.top.width, closeTo(1, 0.01));
      expect(deco.boxShadow, anyOf(isNull, isEmpty), reason: '不要阴影');
      final TextField field = tester.widget<TextField>(find.byKey(const Key('composer-input')));
      final InputBorder? borderStyle = field.decoration?.border;
      expect(borderStyle, InputBorder.none, reason: '内部不再套一层输入框边框');
    });
  });
}