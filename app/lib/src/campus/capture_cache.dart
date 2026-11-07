// t150：抓取成功后缓存 cardId / 学期，供探针与设置页查看。
//
// 背景：登录完成、抓取成功后 WebView 会被关闭，此时**没有活着的控制器**；
// 探针不该因此报错（那是正常状态），应显示"上次抓取缓存下来的值"。
//
// 只放内存（不写盘、不进日志）：cardId 是页面内的用户标识，不是凭据，但仍按最小暴露处理。

import 'package:flutter/foundation.dart';

/// 一次成功抓取留下的标识缓存。
@immutable
class CaptureCacheEntry {
  const CaptureCacheEntry({
    required this.cardId,
    required this.years,
    required this.capturedAt,
  });

  final String cardId;
  final String years;
  final DateTime capturedAt;

  /// 脱敏展示：前 6 位 + … + 后 4 位。
  String get maskedCardId => cardId.length <= 10
      ? '（过短）'
      : '${cardId.substring(0, 6)}…${cardId.substring(cardId.length - 4)}';

  String get capturedAtLabel {
    String two(int value) => value.toString().padLeft(2, '0');
    return '${two(capturedAt.hour)}:${two(capturedAt.minute)}';
  }

  /// 探针/设置里显示的一行文案。
  String get summary => 'cardId: $maskedCardId（来自上次抓取，缓存于 $capturedAtLabel）'
      ' · 学期: $years';
}

/// 进程内缓存（单实例；不落盘）。
class CampusCaptureCache {
  CampusCaptureCache._();

  static final CampusCaptureCache instance = CampusCaptureCache._();

  CaptureCacheEntry? _entry;

  CaptureCacheEntry? get entry => _entry;

  /// 是否有可用缓存。
  bool get hasCardId => _entry != null && _entry!.cardId.isNotEmpty;

  /// 抓取成功（拿到 schedule 模板）时写入。
  void rememberTemplate({required String cardId, required String years, DateTime? now}) {
    if (cardId.trim().isEmpty) {
      return;
    }
    _entry = CaptureCacheEntry(
      cardId: cardId.trim(),
      years: yes(years),
      capturedAt: now ?? DateTime.now(),
    );
  }

  /// 学期可能为空（SPA 未渲染），此时保留上一次的学期。
  String yes(String years) {
    final String trimmed = years.trim();
    if (trimmed.isNotEmpty) {
      return trimmed;
    }
    return _entry?.years ?? '';
  }

  void clear() {
    _entry = null;
  }
}

/// 探针里显示的 cardId 文案：有缓存走缓存，没有就明确"尚未抓取过"。
String describeCachedCardId() {
  final CaptureCacheEntry? entry = CampusCaptureCache.instance.entry;
  if (entry == null || entry.cardId.isEmpty) {
    return 'cardId: 尚未抓取过（需要先成功抓取一次课表，之后这里会显示缓存值）';
  }
  return entry.summary;
}
