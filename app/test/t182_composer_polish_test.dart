// t182：composer 观感两条 —— 占位文字垂直居中 + 输入栏只留一层装饰。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/main.dart';
import 'package:yunong/src/theme/theme_controller.dart';

import 'support/test_window.dart';

Future<void> _pumpApp(WidgetTester tester, Size size) async {
  setTestWindow(tester, size.width, size.height);
  await tester.pumpWidget(YunongApp(
    themeController: ThemeController(store: MemorySettingsStore()),
  ));
  await tester.pumpAndSettle();
}

/// composer 子树里"带装饰的 Container"层数（=0 表示只有外层 1 层，不算 composer 自己）。
int _decoratedContainersInside(WidgetTester tester, Finder composer) {
  final Iterable<Element> all = find
      .descendant(of: composer, matching: find.byType(Container))
      .evaluate();
  int counted = 0;
  for (final Element element in all) {
    final Container box = element.widget as Container;
    if (box.decoration != null || box.color != null) {
      counted++;
    }
  }
  return counted;
}

void main() {
  for (final Size size in const <Size>[Size(1280, 800), Size(360, 800)]) {
    group('${size.width.toInt()}×${size.height.toInt()} composer（t182）', () {
      testWidgets('占位文字与发送键垂直中心差 < 2dp', (WidgetTester tester) async {
        await _pumpApp(tester, size);
        final Offset hint = tester.getCenter(find.text('输入消息…'));
        final Offset send = tester.getCenter(find.byKey(const Key('send-button')));
        final double delta = (hint.dy - send.dy).abs();
        // ignore: avoid_print
        print('SIZE=${size.width.toInt()} hint.dy=${hint.dy} send.dy=${send.dy} delta=$delta');
        expect(delta, lessThan(2), reason: '占位文字应垂直居中（delta=$delta）');
      });

      testWidgets('composer 内只有一层带装饰的 Container（不给内部输入框套底色/边框）',
          (WidgetTester tester) async {
        await _pumpApp(tester, size);
        final int nested = _decoratedContainersInside(tester, find.byKey(const Key('composer')));
        // ignore: avoid_print
        print('SIZE=${size.width.toInt()} nestedDecoratedContainers=$nested');
        expect(nested, 0, reason: '外层 composer 之外不应再有带 decoration/color 的 Container');
      });

      testWidgets('内部 TextField 无填充、无边框', (WidgetTester tester) async {
        await _pumpApp(tester, size);
        final TextField field =
            tester.widget<TextField>(find.byKey(const Key('composer-input')));
        final InputDecoration decoration = field.decoration!;
        expect(decoration.filled, isFalse, reason: '输入框不再自带填充色');
        expect(decoration.fillColor, anyOf(isNull, Colors.transparent));
        expect(decoration.border, InputBorder.none);
        expect(decoration.enabledBorder, InputBorder.none);
        expect(decoration.focusedBorder, InputBorder.none);
      });
    });
  }
}