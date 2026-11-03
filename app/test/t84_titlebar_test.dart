// t84：标题栏跟随主题（FFI 接线）。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/core/titlebar_service.dart';
import 'package:yunong/src/theme/theme_controller.dart';
import 'package:yunong/src/theme/file_settings_store.dart';

class _RecordingTitleBar implements TitleBarBackend {
  _RecordingTitleBar({this.code = ''});

  final String code;
  final List<bool> calls = <bool>[];
  int notFoundTimes = 0;

  @override
  Future<String> setDark(bool dark) async {
    calls.add(dark);
    if (code == 'NOT_FOUND') {
      // 第一次 NOT_FOUND（窗口未建），之后成功
      return calls.length == 1 ? 'NOT_FOUND' : '';
    }
    return code;
  }
}

void main() {
  group('标题栏映射', () {
    test('深蓝玻璃 → dark=true；极简/书本 → dark=false', () {
      expect(titleBarDarkFor(AppThemeId.deepGlass), isTrue);
      expect(titleBarDarkFor(AppThemeId.minimalBlack), isFalse);
      expect(titleBarDarkFor(AppThemeId.paperCoral), isFalse);
    });

    test('syncTitleBar 按主题调用', () async {
      final backend = _RecordingTitleBar();
      await syncTitleBar(backend, AppThemeId.deepGlass, sleep: (_) async {});
      await syncTitleBar(backend, AppThemeId.minimalBlack, sleep: (_) async {});
      expect(backend.calls, <bool>[true, false]);
    });

    test('NOT_FOUND（窗口未建）会重试一次，且不抛错', () async {
      final backend = _RecordingTitleBar(code: 'NOT_FOUND')..notFoundTimes = 1;
      await syncTitleBar(backend, AppThemeId.deepGlass, sleep: (_) async {});
      expect(backend.calls.length, 2, reason: 'NOT_FOUND 应重试一次');
    });

    test('UNSUPPORTED（非 Windows）静默忽略', () async {
      final backend = _RecordingTitleBar(code: 'UNSUPPORTED');
      await syncTitleBar(backend, AppThemeId.paperCoral, sleep: (_) async {});
      expect(backend.calls, <bool>[false]);
    });
  });

  testWidgets('启动时同步一次；切换主题后再次同步（标题栏跟随）', (WidgetTester tester) async {
    final backend = _RecordingTitleBar();
    final controller = ThemeController(store: MemorySettingsStore());
    await tester.pumpWidget(YunongApp(themeController: controller, titleBarBackend: backend));
    await tester.pumpAndSettle();

    expect(backend.calls, isNotEmpty, reason: '启动时必须同步一次');
    final int afterStart = backend.calls.length;

    controller.select(AppThemeId.deepGlass);
    await tester.pumpAndSettle();
    expect(backend.calls.length, greaterThan(afterStart), reason: '切主题后必须再同步');
    expect(backend.calls.last, isTrue, reason: '深蓝玻璃 → 深色标题栏');

    controller.select(AppThemeId.minimalBlack);
    await tester.pumpAndSettle();
    expect(backend.calls.last, isFalse, reason: '切回黑白 → 浅色标题栏');
  });
}
