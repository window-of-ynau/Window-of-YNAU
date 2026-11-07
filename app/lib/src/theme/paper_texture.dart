// 纸纹背景（C 主题专用）：**一次 drawRect** 铺满，不随窗口面积膨胀。
//
// 为什么要有这个文件（性能事故记录，改之前先读）：
// 旧实现是 `Image.asset(repeat: ImageRepeat.repeat, fit: BoxFit.none)` 外包一层 `Opacity`：
//   ① `ImageRepeat` 会按 8×8 的贴图**逐块**绘制 —— 1280×900 视口 = 160×113 = **18,080 次
//      drawImageRect/帧**；2560×1440 = **57,600 次**。实测（软件光栅）光栅耗时从 1.25ms
//      涨到 36.39ms，而同期 A 主题只从 1.34ms 涨到 3.33ms —— 这就是"莫名最卡"的根因；
//   ② `Opacity` 是整屏 `saveLayer`（离屏缓冲），滚动时每帧重建；
//   ③ 贴图层和滚动内容在同一个 Stack 里，没有 RepaintBoundary 隔离，滚动会带着背景一起重绘。
//
// 现在：贴图解码一次 → 缓存 `ui.Image` → `ImageShader(TileMode.repeated)` → **1 次 drawRect**；
// 透明度用 `ColorFilter.matrix` 的 alpha 行做，**不用 Opacity**；外面套 RepaintBoundary，
// 滚动时背景层直接复用，不重绘。

import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show rootBundle;

import 'yunong_tokens.dart';

/// 静态纸纹层：铺在内容下方，不参与滚动重绘。
class PaperTexture extends StatefulWidget {
  const PaperTexture({super.key, required this.spec, required this.child});

  final TextureSpec spec;
  final Widget child;

  @override
  State<PaperTexture> createState() => _PaperTextureState();
}

class _PaperTextureState extends State<PaperTexture> {
  /// 全局只解码一次：同一个 asset 在多处/多次重建时复用同一张 `ui.Image`。
  static final Map<String, Future<ui.Image>> _cache = <String, Future<ui.Image>>{};

  ui.Image? _image;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final Future<ui.Image> future = _cache.putIfAbsent(widget.spec.asset, () async {
      final ByteData data = await rootBundle.load(widget.spec.asset);
      final ui.Codec codec = await ui.instantiateImageCodec(data.buffer.asUint8List());
      final ui.FrameInfo frame = await codec.getNextFrame();
      return frame.image;
    });
    try {
      final ui.Image image = await future;
      if (mounted) {
        setState(() => _image = image);
      }
    } catch (error) {
      // 资源缺失不该让整个界面挂掉：没有纸纹就退回纯纸色（观感损失 <性能风险）
      debugPrint('纸纹加载失败，退回纯色：$error');
      _cache.remove(widget.spec.asset);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Stack(
      fit: StackFit.expand,
      children: <Widget>[
        // 静态层：RepaintBoundary + shouldRepaint=false，滚动时直接复用缓存层
        RepaintBoundary(
          child: CustomPaint(
            painter: _PaperPainter(image: _image, opacity: widget.spec.opacity),
            size: Size.infinite,
            isComplex: false,
            willChange: false,
          ),
        ),
        widget.child,
      ],
    );
  }
}

class _PaperPainter extends CustomPainter {
  _PaperPainter({required this.image, required this.opacity});

  final ui.Image? image;
  final double opacity;

  @override
  void paint(Canvas canvas, Size size) {
    final ui.Image? img = image;
    if (img == null || size.isEmpty) {
      return;
    }
    // 平铺交给 GPU 的 TileMode，一次 drawRect 搞定整屏
    final ui.ImageShader shader = ui.ImageShader(
      img,
      TileMode.repeated,
      TileMode.repeated,
      Float64List.fromList(<double>[1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]),
    );
    final Paint paint = Paint()
      ..shader = shader
      // 用 alpha 行做整体透明度，避免 Opacity → saveLayer
      ..colorFilter = ColorFilter.matrix(<double>[
        1, 0, 0, 0, 0, //
        0, 1, 0, 0, 0, //
        0, 0, 1, 0, 0, //
        0, 0, 0, opacity, 0, //
      ]);
    canvas.drawRect(Offset.zero & size, paint);
  }

  @override
  bool shouldRepaint(_PaperPainter oldDelegate) =>
      oldDelegate.image != image || oldDelegate.opacity != opacity;
}
