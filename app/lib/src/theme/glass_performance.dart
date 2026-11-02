// 玻璃性能闸门（批次 2）。
//
// 只有 B（深蓝玻璃）主题会用到；A/C 的 `tokens.glass == null`，这套逻辑完全不启动，
// 所以对默认主题是**零成本**的。
//
// 降级阶梯（判据来自设计稿，不许放宽）：
//   L3 全玻璃：AppBar + 输入条都真模糊（sigma ≤ 26）
//   L2 半玻璃：只留 AppBar 真模糊，输入条改"假玻璃"
//   L1 假玻璃：全部半透明纯色 + 1px 内高光，零 BackdropFilter
//   L0 纯色  ：连玻璃质感都不要，纯 surface 色
//
// 判据：滚动/使用中 frame time（build+raster）的 **p90 > 18.2ms（≈55fps）** 连续一个窗口
// 就降一档；恢复要等 **p90 < 14ms 且稳定 30s**（迟滞），避免来回抖动。

import 'dart:math' as math;

import 'package:flutter/scheduler.dart';
import 'package:flutter/widgets.dart';

/// 玻璃档位。数值越大越"真"。
enum GlassLevel { l0, l1, l2, l3 }

/// 帧率闸门：把 [GlassLevel] 暴露给 `GlassPanel`。
///
/// 用 `SchedulerBinding.addTimingsCallback` 采样真实帧耗时（build + raster），
/// 不需要额外依赖，也不需要用户手动开关。
class GlassGovernor extends ChangeNotifier {
  GlassGovernor({
    this.window = const Duration(seconds: 3),
    this.downgradeP90Ms = 18.2,
    this.upgradeP90Ms = 14.0,
    this.upgradeHold = const Duration(seconds: 30),
    this.minSamples = 30,
  });

  final Duration window;
  final double downgradeP90Ms;
  final double upgradeP90Ms;
  final Duration upgradeHold;
  final int minSamples;

  GlassLevel _level = GlassLevel.l3;
  GlassLevel get level => _level;

  bool _attached = false;
  final List<double> _framesMs = <double>[];
  DateTime? _lastChange;
  int _windowFrames = 0;

  /// 用户手动锁定档位（设置页"玻璃强度"选非自动时）。
  GlassLevel? _locked;
  GlassLevel? get locked => _locked;

  void lock(GlassLevel? level) {
    _locked = level;
    if (level != null && level != _level) {
      _level = level;
      notifyListeners();
    }
  }

  void attach() {
    if (_attached) {
      return;
    }
    _attached = true;
    SchedulerBinding.instance.addTimingsCallback(_onTimings);
  }

  void detach() {
    if (!_attached) {
      return;
    }
    _attached = false;
    SchedulerBinding.instance.removeTimingsCallback(_onTimings);
    _framesMs.clear();
    _windowFrames = 0;
    _lastChange = null;
  }

  @override
  void dispose() {
    detach();
    super.dispose();
  }

  void _onTimings(List<FrameTiming> timings) {
    final List<double> samples = <double>[
      for (final FrameTiming timing in timings)
        (timing.buildDuration + timing.rasterDuration).inMicroseconds / 1000.0,
    ];
    feed(samples);
  }

  /// 供测试直接喂样本（毫秒）。
  @visibleForTesting
  void feed(List<double> framesMs) {
    _framesMs.addAll(framesMs);
    _windowFrames += framesMs.length;
    // 只保留一个窗口内的样本（按 60fps 估算上限 3*60*4 帧，防内存增长）
    final int cap = math.max(minSamples * 4, 720);
    if (_framesMs.length > cap) {
      _framesMs.removeRange(0, _framesMs.length - cap);
    }
    // 每积累一个窗口的量再评估一次
    if (_windowFrames < minSamples) {
      return;
    }
    _windowFrames = 0;
    _evaluate();
  }

  void _evaluate() {
    if (_framesMs.isEmpty) {
      return;
    }
    final double p90 = _percentile(_framesMs, 0.90);
    final DateTime now = DateTime.now();
    final GlassLevel? lock = _locked;
    if (lock != null) {
      // 手动锁定档位时不自动降级（用户明确选了"磨砂"就给他磨砂，掉帧由用户自己判断）
      return;
    }
    if (p90 > downgradeP90Ms && _level != GlassLevel.l0) {
      _level = GlassLevel.values[_level.index - 1];
      _lastChange = now;
      _framesMs.clear();
      notifyListeners();
      return;
    }
    final bool recovered = p90 < upgradeP90Ms &&
        _level != GlassLevel.l3 &&
        (_lastChange == null || now.difference(_lastChange!) >= upgradeHold);
    if (recovered) {
      _level = GlassLevel.values[_level.index + 1];
      _lastChange = now;
      _framesMs.clear();
      notifyListeners();
    }
  }

  static double _percentile(List<double> values, double p) {
    final List<double> sorted = List<double>.of(values)..sort();
    final int index = ((sorted.length - 1) * p).round().clamp(0, sorted.length - 1);
    return sorted[index];
  }

  /// 真实帧耗时 p90（调试/设置页展示用）。
  double? get lastP90Ms {
    if (_framesMs.isEmpty) {
      return null;
    }
    return _percentile(_framesMs, 0.90);
  }
}

/// 把当前玻璃档位传下去；没有 Scope 时按 L3 处理（真机默认）。
class GlassScope extends InheritedNotifier<GlassGovernor> {
  const GlassScope({super.key, required GlassGovernor governor, required super.child})
      : super(notifier: governor);

  static GlassLevel levelOf(BuildContext context) {
    final GlassScope? scope = context.dependOnInheritedWidgetOfExactType<GlassScope>();
    return scope?.notifier?.level ?? GlassLevel.l3;
  }
}

