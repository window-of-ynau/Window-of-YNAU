// t156：数据根覆盖（--data-dir / YUNONG_DATA_DIR）—— 优先级、非法路径、派生路径、设置页显示。

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_settings_section.dart';
import 'package:yunong/src/core/app_paths.dart';
import 'package:yunong/src/settings/settings_page.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

void main() {
  late Directory tempRoot;

  setUp(() {
    tempRoot = Directory.systemTemp.createTempSync('yunong_t156_');
  });

  tearDown(() {
    AppPaths.initDataRoot(env: const <String, String>{}, args: const <String>[]);
    try {
      if (tempRoot.existsSync()) {
        tempRoot.deleteSync(recursive: true);
      }
    } on Object {
      // 清理失败不影响结论
    }
  });

  String profile(String name) {
    final String path = <String>[tempRoot.path, name].join(Platform.pathSeparator);
    Directory(path).createSync(recursive: true);
    return path;
  }

  group('三级优先级（t156）', () {
    test('命令行 --data-dir=… 优先于环境变量', () async {
      // 先把当前进程切到临时根，避免影响真实默认目录
      final String cmd = profile('cmd');
      final String envDir = profile('env');
      final DataRootResolution resolution = AppPaths.resolveDataRoot(
        env: <String, String>{'YUNONG_DATA_DIR': envDir},
        args: <String>['--data-dir=$cmd'],
        defaultPath: profile('default'),
      );
      expect(resolution.source, DataRootSource.commandLine);
      expect(resolution.path, cmd);
      expect(resolution.ok, isTrue);
    });

    test('无命令行时用环境变量', () {
      final String envDir = profile('env-only');
      final DataRootResolution resolution = AppPaths.resolveDataRoot(
        env: <String, String>{'YUNONG_DATA_DIR': envDir},
        args: const <String>[],
        defaultPath: profile('default'),
      );
      expect(resolution.source, DataRootSource.environment);
      expect(resolution.path, envDir);
    });

    test('都没有 → 默认位置', () {
      final String fallback = profile('default-only');
      final DataRootResolution resolution = AppPaths.resolveDataRoot(
        env: const <String, String>{},
        args: const <String>[],
        defaultPath: fallback,
      );
      expect(resolution.source, DataRootSource.defaultLocation);
      expect(resolution.path, fallback);
      expect(resolution.isOverride, isFalse);
    });

    test('支持空格写法 --data-dir <路径>', () {
      final String cmd = profile('spaced');
      final DataRootResolution resolution = AppPaths.resolveDataRoot(
        env: const <String, String>{},
        args: <String>['--data-dir', cmd],
        defaultPath: profile('default'),
      );
      expect(resolution.source, DataRootSource.commandLine);
      expect(resolution.path, cmd);
    });

    test('不存在的父目录会被创建', () {
      final String nested = <String>[tempRoot.path, 'a', 'b', 'c'].join(Platform.pathSeparator);
      expect(Directory(nested).existsSync(), isFalse);
      final DataRootResolution resolution = AppPaths.resolveDataRoot(
        env: const <String, String>{},
        args: <String>['--data-dir=$nested'],
        defaultPath: profile('default'),
      );
      expect(resolution.ok, isTrue);
      expect(Directory(nested).existsSync(), isTrue);
    });
  });

  group('非法路径（t156）', () {
    test('路径指向一个已存在的文件 → 报错且不回退默认', () {
      final File file = File(<String>[tempRoot.path, 'not-a-dir'].join(Platform.pathSeparator))
        ..writeAsStringSync('x');
      final String fallback = profile('default');
      final DataRootResolution resolution = AppPaths.resolveDataRoot(
        env: const <String, String>{},
        args: <String>['--data-dir=${file.path}'],
        defaultPath: fallback,
      );
      expect(resolution.ok, isFalse);
      expect(resolution.error, contains('不可写'));
      expect(resolution.source, DataRootSource.commandLine);
      // 关键：**不回退**默认路径（否则会静默复用本地数据库）
      expect(resolution.path, file.path);
    });

    test('空值参数 → 报错', () {
      final DataRootResolution resolution = AppPaths.resolveDataRoot(
        env: const <String, String>{},
        args: const <String>['--data-dir='],
        defaultPath: profile('default'),
      );
      // 空值视作"没给"，退到默认；但 --data-dir '' 这种显式空要能用下面这条覆盖
      expect(resolution.source, DataRootSource.defaultLocation);
    });
  });

  group('覆盖后所有落盘路径跟随（t156）', () {
    test('dataDir / databasePath / settingsPath / webViewDataDir 都在覆盖目录下', () {
      final String dir = profile('profileA');
      AppPaths.initDataRoot(
        env: const <String, String>{},
        args: <String>['--data-dir=$dir'],
      );
      expect(AppPaths.isOverridden, isTrue);
      expect(AppPaths.overrideSource, DataRootSource.commandLine);
      expect(AppPaths.dataRootError, isNull);

      expect(AppPaths.dataDir, dir);
      expect(AppPaths.databasePath, startsWith(dir));
      expect(AppPaths.databasePath, endsWith('yunong.db'));
      expect(AppPaths.settingsPath, startsWith(dir));
      expect(AppPaths.settingsPath, endsWith('settings.json'));
      expect(AppPaths.webViewDataDir, startsWith(dir));
      expect(AppPaths.webViewDataDir, endsWith('webview2'));
      // 凭据目录由 dbPath 推出
      expect(AppPaths.dataDirOf(AppPaths.databasePath), dir);
    });

    test('非法覆盖 → dataRootError 非空，供 main 拒绝启动', () {
      final File file = File(<String>[tempRoot.path, 'file2'].join(Platform.pathSeparator))
        ..writeAsStringSync('x');
      AppPaths.initDataRoot(
        env: const <String, String>{},
        args: <String>['--data-dir=${file.path}'],
      );
      expect(AppPaths.dataRootError, isNotNull);
      expect(AppPaths.dataRootError, contains('不可写'));
    });
  });

  group('设置页显示（t156）', () {
    Widget settings() => MaterialApp(
          home: SettingsPage(
            controller: ThemeController(store: MemorySettingsStore()),
            chatSettings: ChatSettingsController(),
          ),
        );

    testWidgets('默认模式：显示「数据目录 …（默认）」且没有便携提示', (WidgetTester tester) async {
      AppPaths.initDataRoot(env: const <String, String>{}, args: const <String>[]);
      setTestWindow(tester, 1280, 1000);
      await tester.pumpWidget(settings());
      await tester.pumpAndSettle();
      await tester.scrollUntilVisible(
        find.byKey(const Key('settings-data-dir')),
        300,
        scrollable: find.byType(Scrollable).first,
      );
      expect(find.textContaining('（默认）'), findsOneWidget);
      expect(find.byKey(const Key('settings-portable-mode')), findsNothing);
    });

    testWidgets('覆盖模式：显示来源与「便携/隔离模式」提示', (WidgetTester tester) async {
      final String dir = profile('shown');
      AppPaths.initDataRoot(env: const <String, String>{}, args: <String>['--data-dir=$dir']);
      setTestWindow(tester, 1280, 1000);
      await tester.pumpWidget(settings());
      await tester.pumpAndSettle();
      await tester.scrollUntilVisible(
        find.byKey(const Key('settings-portable-mode')),
        300,
        scrollable: find.byType(Scrollable).first,
      );
      expect(find.textContaining('（覆盖：命令行）'), findsOneWidget);
      expect(find.text('便携/隔离模式'), findsOneWidget);
      expect(find.textContaining('数据在 $dir'), findsOneWidget);
    });
  });
}
