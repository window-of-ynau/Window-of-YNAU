// t120：证明「setSurfaceSize 不改 MediaQuery」这个坑，以及 setTestWindow 才是对的。
//
// 这两条测试本身是**回归护栏**：以后谁再改回 setSurfaceSize，第一条不会失败（它只是记录事实），
// 但任何用 setTestWindow 的窄窗用例都会真实生效；若有人误用旧 API，第二条会提醒我们它无效。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/test_window.dart';

Future<Size> _mq(WidgetTester tester) async {
  late Size size;
  await tester.pumpWidget(Builder(builder: (BuildContext context) {
    size = MediaQuery.sizeOf(context);
    return const SizedBox.shrink();
  }));
  return size;
}

void main() {
  testWidgets('setTestWindow(480,900) 真正改变 MediaQuery', (WidgetTester tester) async {
    setTestWindow(tester, 480, 900);
    expect(await _mq(tester), const Size(480, 900));
  });

  testWidgets('旧写法 setSurfaceSize 不改 MediaQuery（这就是假绿来源）', (WidgetTester tester) async {
    await tester.binding.setSurfaceSize(const Size(480, 900));
    addTearDown(() => tester.binding.setSurfaceSize(null));
    expect(await _mq(tester), isNot(const Size(480, 900)),
        reason: '若这条开始失败，说明 Flutter 行为变了：可以回到 setSurfaceSize，但需重新评估所有窄窗用例');
  });
}
