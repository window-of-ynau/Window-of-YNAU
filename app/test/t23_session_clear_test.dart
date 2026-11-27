// security M3：设置页清除校务会话 + 文档路径修正的行为回归。
//
// 覆盖：删除 WebView2 用户数据目录、绝不删课表/记忆库、插件 clear 回调被触发、
// 路径护栏拒绝误删数据根、设置页二次确认与成功提示。

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/session_clear.dart';
import 'package:yunong/src/core/app_paths.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

void main() {
  late Directory temp;

  setUp(() {
    temp = Directory.systemTemp.createTempSync('yunong_t23_');
    AppPaths.initDataRoot(
      env: const <String, String>{},
      args: <String>['--data-dir=${temp.path}'],
    );
  });

  tearDown(() {
    AppPaths.initDataRoot(env: const <String, String>{}, args: const <String>[]);
    try {
      if (temp.existsSync()) {
        temp.deleteSync(recursive: true);
      }
    } on Object {
      // 清理失败不影响结论
    }
  });

  test('清除 WebView2 会话目录，SQLite（yunong.db / timetable.db）保持原样', () async {
    final String sep = Platform.pathSeparator;
    final File cookies =
        File('${AppPaths.webViewDataDir}${sep}EBWebView${sep}Default${sep}Cookies')
          ..createSync(recursive: true)
          ..writeAsStringSync('session-secret');
    final File db = File(AppPaths.databasePath)
      ..createSync(recursive: true)
      ..writeAsStringSync('DB-DATA');
    final File timetable = File('${AppPaths.dataDir}${sep}timetable.db')
      ..createSync(recursive: true)
      ..writeAsStringSync('COURSES');

    int cookieCalls = 0;
    final CampusSessionClearResult result =
        await CampusSessionClearService(clearCookies: () async {
      cookieCalls += 1;
      return true;
    }).clear();

    expect(result.ok, isTrue, reason: '${result.error}');
    expect(result.removedEntries, greaterThan(0));
    expect(result.cookiesCleared, isTrue);
    expect(cookieCalls, 1, reason: '插件 clear 必须被调用');
    expect(Directory(AppPaths.webViewDataDir).existsSync(), isFalse);
    expect(cookies.existsSync(), isFalse, reason: '会话文件必须被删除');
    expect(db.readAsStringSync(), 'DB-DATA', reason: '不得删数据库');
    expect(timetable.readAsStringSync(), 'COURSES', reason: '不得删课表库');
  });

  test('重复清除是安全的（目录不存在仍返回成功，不触碰数据库）', () async {
    final File db = File(AppPaths.databasePath)
      ..createSync(recursive: true)
      ..writeAsStringSync('DB-DATA');

    final CampusSessionClearResult result = await CampusSessionClearService(
      clearCookies: () async => false,
    ).clear();

    expect(result.ok, isTrue);
    expect(result.removedEntries, 0);
    expect(Directory(AppPaths.webViewDataDir).existsSync(), isFalse);
    expect(db.readAsStringSync(), 'DB-DATA');
  });

  test('路径护栏：拒绝把数据根当会话目录删除，且不调用插件', () async {
    final File db = File(AppPaths.databasePath)
      ..createSync(recursive: true)
      ..writeAsStringSync('DB-DATA');
    int cookieCalls = 0;

    final CampusSessionClearResult result = await CampusSessionClearService(
      webViewDataDir: AppPaths.dataDir,
      databasePath: AppPaths.databasePath,
      clearCookies: () async {
        cookieCalls += 1;
        return true;
      },
    ).clear();

    expect(result.ok, isFalse);
    expect(result.error, contains('拒绝清除'));
    expect(cookieCalls, 0);
    expect(Directory(AppPaths.dataDir).existsSync(), isTrue);
    expect(db.readAsStringSync(), 'DB-DATA');
  });

  testWidgets('设置页：二次确认 → 清除 → 成功提示，且插件被调用', (WidgetTester tester) async {
    setTestWindow(tester, 1000, 900);
    int cookieCalls = 0;
    final CampusSessionClearService service = CampusSessionClearService(
      webViewDataDir: '${AppPaths.dataDir}${Platform.pathSeparator}webview2',
      databasePath: AppPaths.databasePath,
      clearCookies: () async {
        cookieCalls += 1;
        return true;
      },
    );
    await tester.pumpWidget(MaterialApp(
      home: SettingsPage(
        controller: ThemeController(store: MemorySettingsStore()),
        sessionClearService: service,
      ),
    ));
    await tester.pumpAndSettle();

    await tester.scrollUntilVisible(
      find.byKey(const Key('clear-campus-session')),
      300,
      scrollable: find.byType(Scrollable).first,
    );
    await tester.tap(find.byKey(const Key('clear-campus-session')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('clear-campus-session-dialog')), findsOneWidget);
    // 说明文案必须交代「会退出校务登录，课表数据不受影响」
    expect(find.textContaining('课表数据不受影响'), findsOneWidget);

    await tester.tap(find.byKey(const Key('clear-campus-session-confirm')));
    await tester.pumpAndSettle();

    expect(find.byKey(const Key('clear-campus-session-result')), findsOneWidget);
    expect(find.text('校务登录态已清除'), findsOneWidget);
    expect(cookieCalls, 1);
  });
}
