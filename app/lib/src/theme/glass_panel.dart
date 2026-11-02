// 玻璃面板：**全项目唯一**构造 `BackdropFilter` 的地方。
//
// 规则（是 review 的检查项）：
// 1. `tokens.glass == null`（A/C）→ 直接返回普通 `DecoratedBox`，**不 import 也不用 blur**；
// 2. 只有 B 主题 + `allowBackdropFilter == true` + 闸门档位允许时，才会出现 `BackdropFilter`；
// 3. 每屏 ≤ 2 层：只有 `GlassRole.appBar` 与 `GlassRole.composer` 会真模糊，
//    课程块/设置卡片等一律 `GlassRole.flat`（半透明纯色 + 1px 内高光，零 blur）。

import 'dart:ui' show ImageFilter;

import 'package:flutter/material.dart';

import 'glass_performance.dart';
import 'yunong_tokens.dart';

/// 玻璃面板的角色：决定它在降级时先牺牲谁。
enum GlassRole {
  /// 顶栏：L2 时保留模糊（可读性最依赖它）。
  appBar,

  /// 输入条：L2 时先降成假玻璃。
  composer,

  /// 卡片/课程块等：**永远不模糊**。
  flat,
}

class GlassPanel extends StatelessWidget {
  const GlassPanel({
    super.key,
    required this.child,
    this.role = GlassRole.flat,
    this.borderRadius,
    this.padding,
    this.showTopHighlight = true,
  });

  final Widget child;
  final GlassRole role;
  final BorderRadius? borderRadius;
  final EdgeInsetsGeometry? padding;
  final bool showTopHighlight;

  @override
  Widget build(BuildContext context) {
    final YunongTokens t = context.tokens;
    final GlassSpec? spec = t.glass;
    final BorderRadius radius = borderRadius ?? BorderRadius.circular(t.radiusM);

    // A/C：没有玻璃，普通表面（这条分支保证 A/C 全树 0 个 BackdropFilter）
    if (spec == null) {
      return DecoratedBox(
        decoration: BoxDecoration(
          color: t.surface,
          borderRadius: radius,
          border: Border.all(color: t.hairline, width: t.borderWidth),
          boxShadow: t.shadowPanel,
        ),
        child: Padding(padding: padding ?? EdgeInsets.zero, child: child),
      );
    }

    final GlassLevel level = GlassScope.levelOf(context);
    final bool blurAllowed = _blurAllowed(level, role);

    Widget panel = DecoratedBox(
      decoration: BoxDecoration(
        color: spec.tint,
        borderRadius: radius,
        gradient: spec.innerGlow,
        border: showTopHighlight
            ? Border(top: BorderSide(color: spec.highlightTop, width: 1))
            : null,
        boxShadow: t.shadowPanel,
      ),
      child: Padding(padding: padding ?? EdgeInsets.zero, child: child),
    );

    if (blurAllowed) {
      // 模糊必须在 ClipRRect 里，否则会把整块画布都糊掉（也会拖慢渲染）
      panel = ClipRRect(
        borderRadius: radius,
        child: BackdropFilter(
          filter: ImageFilter.blur(sigmaX: spec.blurSigma, sigmaY: spec.blurSigma),
          child: panel,
        ),
      );
    } else {
      panel = ClipRRect(borderRadius: radius, child: panel);
    }
    return panel;
  }

  /// 降级阶梯：L3 全开；L2 只留顶栏；L1/L0 一律假玻璃。
  bool _blurAllowed(GlassLevel level, GlassRole role) {
    if (role == GlassRole.flat) {
      return false; // 卡片永远不模糊
    }
    switch (level) {
      case GlassLevel.l3:
        return true;
      case GlassLevel.l2:
        return role == GlassRole.appBar;
      case GlassLevel.l1:
      case GlassLevel.l0:
        return false;
    }
  }
}
