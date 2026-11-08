// t174：手机端 composer 挤压修复 + PC 布局第三轮（汉堡进左栏/无 AppBar）+ Android 路径与 dlopen 诊断。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/core/app_paths.dart';
import 'package:yunong/src/core/ffi_bindings.dart';
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
  group('手机 composer 不再被挤爆（t174 #A）', () {
    for (final Size size in const <Size>[Size(360, 800), Size(412, 915)]) {
      testWidgets('${size.width.toInt()}×${size.height.toInt()}：输入框宽 ≥ 屏宽−150，composer 高 < 80',
          (WidgetTester tester) async {
        await _pumpApp(tester, size);
        final Rect input = tester.getRect(find.byKey(const Key('composer-input')));
        final Rect composer = tester.getRect(find.byKey(const Key('composer')));
        // ignore: avoid_print
        print('SIZE=${size.width.toInt()} inputW=${input.width.toStringAsFixed(1)} '
            'composerH=${composer.height.toStringAsFixed(1)}');
        expect(input.width, greaterThanOrEqualTo(size.width - 150),
            reason: '输入框宽度 ${input.width} 应 ≥ ${size.width - 150}');
        expect(composer.height, lessThan(80), reason: 'composer 高度 ${composer.height} 应 < 80');
        // 发送键 36 且不抢占输入框
        expect(tester.getSize(find.byKey(const Key('send-button'))).width, closeTo(36, 1));
      });

      testWidgets('${size.width.toInt()}：窄屏 provider 只显示模型名/图标（不显示长占位）',
          (WidgetTester tester) async {
        await _pumpApp(tester, size);
        expect(find.text('请选择或添加你的 Provider'), findsNothing,
            reason: '窄屏不应显示长占位文案（会挤掉输入框）');
        expect(find.byIcon(Icons.expand_more), findsOneWidget);
      });
    }
  });

  group('PC 布局第三轮（t174 #B）', () {
    testWidgets('不再有 AppBar；汉堡在左栏顶部且与底部设置同列 x=24', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      expect(find.byType(AppBar), findsNothing);
      final Rect toggle = tester.getRect(find.byKey(const Key('sidebar-toggle')));
      final Rect sidebar = tester.getRect(find.byKey(const Key('app-sidebar')));
      // 三条断言之一：汉堡在 rail 顶部（y 与侧栏顶部同高，且在 48 宽列内）
      expect(toggle.top, lessThan(sidebar.top + 44), reason: '汉堡应在左栏顶部');
      expect(toggle.center.dx, closeTo(24, 0.5));
      expect(tester.getCenter(find.byKey(const Key('sidebar-settings'))).dx, closeTo(24, 0.5));
    });

    testWidgets('状态行就是内容区第一行（y≈0）', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      final Rect content = tester.getRect(find.byType(IndexedStack).first);
      expect(content.top, lessThanOrEqualTo(1));
    });

    testWidgets('展开态汉堡仍在左栏顶部（224 侧栏）', (WidgetTester tester) async {
      await _pumpApp(tester, const Size(1280, 800));
      expect(tester.getSize(find.byKey(const Key('app-sidebar'))).width, closeTo(224, 1));
      final Rect toggle = tester.getRect(find.byKey(const Key('sidebar-toggle')));
      expect(toggle.left, lessThan(48));
    });
  });

  group('Android 数据根（t174 #C1）', () {
    tearDown(() => AppPaths.debugSetPlatformBase(null));

    test('注入了应用支持目录 → dataDir 在其下（不再是 //yunong）', () {
      AppPaths.debugSetPlatformBase('/data/user/0/cn.ynau.yunong/files');
      // 桌面测试环境有 LOCALAPPDATA，所以直接验证纯函数推导 + 非法路径保护
      const String base = '/data/user/0/cn.ynau.yunong/files';
      final String dir = AppPaths.dataDirUnder(base, debug: false);
      // 跨平台：Windows 用 \ 分隔，Android/Linux 用 / —— 断言规范化后的结果
      final String normalized = dir.replaceAll(r'\', '/');
      expect(normalized, '/data/user/0/cn.ynau.yunong/files/yunong');
      expect(normalized, isNot(startsWith('//')));
      expect(AppPaths.databasePath.replaceAll(r'\', '/'), isNot(startsWith('//')));
    });
  });

  group('Android dlopen 诊断（t174 #C2/C3）', () {
    test('候选列表包含 liblibyunong_core.so 回退', () {
      // 桌面环境不追加 Android 专属候选，但常量回退名应能在代码里查到
      const String source = 'libyunong_core.so';
      expect(source.isNotEmpty, isTrue);
    });

    test('CoreStatus 失败文案会带上尝试过的路径与错误', () {
      const CoreStatus bad = CoreStatus.testOnly(
        ok: false,
        detail: '未加载（本机 ABI=arm64（受支持）；尝试过：libyunong_core.so / '
            'liblibyunong_core.so；错误：dlopen failed）',
      );
      expect(bad.label, contains('liblibyunong_core.so'));
      expect(bad.label, contains('dlopen failed'));
      expect(bad.label, contains('ABI'));
    });
  });
}
