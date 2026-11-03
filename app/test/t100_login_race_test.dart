// t100：首次登录失败的竞态（导航后 JS 状态重置 vs Dart 游标）+ 工具栏对比度。

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/campus/webview_fetcher.dart';
import 'package:yunong/src/theme/theme_controller.dart';

/// WCAG 对比度（1:1 ~ 21:1）。
double contrastRatio(Color a, Color b) {
  final double la = a.computeLuminance();
  final double lb = b.computeLuminance();
  final double hi = math.max(la, lb);
  final double lo = math.min(la, lb);
  return (hi + 0.05) / (lo + 0.05);
}

void main() {
  group('P0 竞态：导航后游标必须重置', () {
    test('sessionId 变化（新文档）→ 游标归零', () {
      expect(
        nextConsumedCursor(consumed: 1, eventCount: 1, sessionId: 'new', lastSessionId: 'old'),
        0,
      );
    });

    test('events 变短（被重置）→ 游标归零', () {
      expect(nextConsumedCursor(consumed: 3, eventCount: 1, sessionId: 'same', lastSessionId: 'same'), 0);
    });

    test('同一会话、未变短 → 游标保持（不重复消费）', () {
      expect(nextConsumedCursor(consumed: 2, eventCount: 5, sessionId: 'same', lastSessionId: 'same'), 2);
    });

    test('首次拉取（无 lastSessionId）→ 归零', () {
      expect(nextConsumedCursor(consumed: 0, eventCount: 1, sessionId: 's1'), 0);
    });

    test('注入脚本带 sessionId（Dart 才有东西可比）', () {
      expect(kPortalScheduleScript.contains('sessionId'), isTrue);
    });
  });

  group('P1 工具栏对比度', () {
    test('正文（onSurface on surfaceContainerHigh）≥ 4.5:1，三套主题 × 明暗', () {
      for (final AppThemeId id in AppThemeId.values) {
        for (final Brightness brightness in <Brightness>[Brightness.light, Brightness.dark]) {
          final ThemeData theme = ThemeController(store: MemorySettingsStore(), initialTheme: id).theme(brightness);
          final double ratio = contrastRatio(
            theme.colorScheme.onSurface,
            theme.colorScheme.surfaceContainerHigh,
          );
          expect(ratio, greaterThanOrEqualTo(4.5),
              reason: '$id/$brightness 正文对比度 $ratio 低于 4.5:1');
        }
      }
    });

    test('主按钮（onPrimary on primary）≥ 4.5:1，三套主题 × 明暗', () {
      for (final AppThemeId id in AppThemeId.values) {
        for (final Brightness brightness in <Brightness>[Brightness.light, Brightness.dark]) {
          final ThemeData theme = ThemeController(store: MemorySettingsStore(), initialTheme: id).theme(brightness);
          final double ratio = contrastRatio(theme.colorScheme.onPrimary, theme.colorScheme.primary);
          expect(ratio, greaterThanOrEqualTo(4.5),
              reason: '$id/$brightness 按钮对比度 $ratio 低于 4.5:1');
        }
      }
    });
  });
}
