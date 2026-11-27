// t2：composer 占位文字垂直居中 —— **真实字体**下的截图级（像素墨迹）回归。
//
// 为什么不能只靠 t182：t182 在 flutter_test 默认的替身字体（Ahem）下跑，
// Ahem 的度量与真实中文字体不同，所以「视觉偏上」在小方块字体上测不出来（假绿）。
// 这里把真实字体用 FontLoader 注入，渲染一帧后**读像素**：取输入框区域里字形墨迹的
// 上下边界算中心，和圆形发送键中心比较；1280 / 360 两个宽度都要 ≤ 1.5px。
//
// 本机没有可用中文字体时（例如精简的 Linux CI）整体 skip，不制造假红；
// 可用 YUNONG_TEST_FONT 指向任意 .ttf/.otf 强制指定字体。

import 'dart:io';
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart' show FontLoader;
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/local_search_service.dart';
import 'package:yunong/src/core/provider_store.dart';
import 'package:yunong/src/core/search_service.dart';
import 'package:yunong/src/theme/theme_controller.dart';

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
}

class _NoopSearchBackend implements SearchBackend {
  @override
  Map<String, dynamic> status({
    required String channel,
    required String protocol,
    required String dbPath,
  }) =>
      <String, dynamic>{'source': 'none', 'label': '未配置', 'available': false, 'hint': ''};
}

class _NoopLocalSearchBackend implements LocalSearchBackend {
  @override
  List<Map<String, dynamic>> search({
    required String query,
    required int limit,
    required String dbPath,
  }) =>
      <Map<String, dynamic>>[];
}

/// 真实字体候选（只收**含中日韩字形**的字体，避免用 DejaVu 之类量出假结论）。
const List<String> _fontCandidates = <String>[
  r'C:\Windows\Fonts\NotoSansSC-VF.ttf',
  r'C:\Windows\Fonts\Deng.ttf',
  r'C:\Windows\Fonts\msyh.ttc',
  r'C:\Windows\Fonts\simhei.ttf',
  '/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc',
  '/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc',
  '/usr/share/fonts/opentype/source-han-sans/SourceHanSansSC-Regular.otf',
  '/System/Library/Fonts/PingFang.ttc',
  '/System/Library/Fonts/Supplemental/Songti.ttc',
];

/// 加载到固定 family `T2RealCJK`；找不到返回 null（调用方 skip）。
Future<String?> _loadRealCjkFont() async {
  final List<String> paths = <String>[
    if ((Platform.environment['YUNONG_TEST_FONT'] ?? '').trim().isNotEmpty)
      Platform.environment['YUNONG_TEST_FONT']!.trim(),
    ..._fontCandidates,
  ];
  for (final String path in paths) {
    final File file = File(path);
    if (!file.existsSync()) {
      continue;
    }
    try {
      final Uint8List bytes = await file.readAsBytes();
      final FontLoader loader = FontLoader('T2RealCJK')
        ..addFont(Future<ByteData>.value(ByteData.sublistView(bytes)));
      await loader.load();
      return 'T2RealCJK';
    } on Object {
      // .ttc 在部分平台加载不了：换下一个
      continue;
    }
  }
  return null;
}

/// 模拟生产主题的关键度量：正文字号 14、行高 1.72（见 themes.dart 的 bodyHeight）。
ThemeData _theme(String family) {
  final ThemeData base = ThemeData(useMaterial3: true);
  final TextTheme themed = base.textTheme.apply(fontFamily: family);
  final TextTheme withHeight = themed.copyWith(
    bodyLarge: themed.bodyLarge!.copyWith(height: 1.72),
    bodyMedium: themed.bodyMedium!.copyWith(height: 1.72),
    bodySmall: themed.bodySmall!.copyWith(height: 1.72),
  );
  return base.copyWith(textTheme: withHeight, primaryTextTheme: withHeight);
}

Future<void> _pump(WidgetTester tester, Size size, String family) async {
  tester.view.physicalSize = size;
  tester.view.devicePixelRatio = 1.0;
  addTearDown(tester.view.reset);
  final ProviderStore store = ProviderStore(store: MemorySettingsStore());
  await tester.pumpWidget(RepaintBoundary(
    key: const Key('t2-root'),
    child: MaterialApp(
      theme: _theme(family),
      home: Scaffold(
        body: ChatPage(
          service: ChatService(backend: _NoopChatBackend()),
          searchService: SearchService(backend: _NoopSearchBackend()),
          localSearchService: LocalSearchService(backend: _NoopLocalSearchBackend()),
          providerStore: store,
        ),
      ),
    ),
  ));
  await tester.pumpAndSettle();
}

/// 在输入框矩形内找「字形墨迹」的上下边界（等价于对一张截图做像素扫描）。
Future<({double center, double top, double bottom, int count})> _inkIn(
    WidgetTester tester, Size size, Rect region) async {
  final RenderRepaintBoundary boundary =
      tester.renderObject<RenderRepaintBoundary>(find.byKey(const Key('t2-root')));
  final ByteData? data = await tester.runAsync<ByteData?>(() async {
    final ui.Image image = await boundary.toImage(pixelRatio: 1.0);
    final ByteData? bd = await image.toByteData(format: ui.ImageByteFormat.rawRgba);
    image.dispose();
    return bd;
  });
  final Uint8List bytes = data!.buffer.asUint8List();
  final int w = size.width.toInt();
  final int h = size.height.toInt();
  int minY = 1 << 30;
  int maxY = -1;
  int count = 0;
  final int x0 = region.left.ceil() + 1;
  final int x1 = region.right.floor() - 1;
  final int y0 = region.top.ceil() + 1;
  final int y1 = region.bottom.floor() - 1;
  for (int y = y0; y < y1 && y < h; y++) {
    for (int x = x0; x < x1 && x < w; x++) {
      final int i = (y * w + x) * 4;
      final int r = bytes[i];
      final int g = bytes[i + 1];
      final int b = bytes[i + 2];
      final int a = bytes[i + 3];
      if (a < 20) {
        continue;
      }
      final double lum = 0.299 * r + 0.587 * g + 0.114 * b;
      if (lum < 200) {
        if (y < minY) minY = y;
        if (y > maxY) maxY = y;
        count++;
      }
    }
  }
  if (maxY < 0) {
    return (center: double.nan, top: double.nan, bottom: double.nan, count: 0);
  }
  return (
    center: (minY + maxY) / 2.0,
    top: minY.toDouble(),
    bottom: maxY.toDouble(),
    count: count,
  );
}

void main() {
  for (final Size size in const <Size>[Size(1280, 800), Size(360, 800)]) {
    testWidgets('真实字体：占位文字与发送键垂直中心差 ≤ 1.5px（${size.width.toInt()} 宽）',
        (WidgetTester tester) async {
      final String? family = await tester.runAsync<String?>(_loadRealCjkFont);
      if (family == null) {
        markTestSkipped('本机无可用中文字体（可设 YUNONG_TEST_FONT 指定 .ttf/.otf）');
        return;
      }
      await _pump(tester, size, family);

      final Rect input = tester.getRect(find.byKey(const Key('composer-input')));
      final Rect send = tester.getRect(find.byKey(const Key('send-button')));
      final ink = await _inkIn(tester, size, input);

      // ignore: avoid_print
      print('T2 W=${size.width.toInt()} input=(${input.top.toStringAsFixed(1)}..'
          '${input.bottom.toStringAsFixed(1)}) ink=(${ink.top.toStringAsFixed(1)}..'
          '${ink.bottom.toStringAsFixed(1)}) inkCenter=${ink.center.toStringAsFixed(2)} '
          'sendCenter=${send.center.dy.toStringAsFixed(2)} px=${ink.count}');

      expect(ink.count, greaterThan(50),
          reason: '真实字形必须真的渲染出来（像素太少说明没量到文字）');
      final double delta = (ink.center - send.center.dy).abs();
      expect(delta, lessThanOrEqualTo(1.5),
          reason: '占位文字应垂直居中：墨迹中心 ${ink.center} vs 发送键中心 ${send.center.dy}，差 $delta');
    });
  }
}
