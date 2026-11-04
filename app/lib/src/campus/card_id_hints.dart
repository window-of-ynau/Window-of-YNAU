// 【t142】cardId 候选的 Dart 侧解析与脱敏（纯数据，不发请求、不读 Cookie）。
//
// 页面脚本 `kCardIdProbeInstallScript` 返回 `JSON.stringify([{v, s}, …])`：
//   v = 32 位 hex 的完整值（页面内标识，不是凭据）
//   s = 来源（`window.xxx` / `xhr:/api/...` / `html` / `localStorage.xxx` / `sessionStorage.xxx`）

import 'dart:convert';

/// 一个候选 cardId 及其来源。
class CardIdHint {
  const CardIdHint({required this.value, required this.source});

  final String value;
  final String source;

  /// 脱敏展示：前 6 位 + `…` + 后 4 位。
  String get masked => maskCardIdValue(value);

  bool get looksLikeCardId => RegExp(r'^[0-9a-fA-F]{32}$').hasMatch(value);

  @override
  String toString() => 'CardIdHint($masked from $source)';
}

/// 脱敏：`a1b2c3…d4e5`。值本身就是"页面内标识"，但仍不整段铺在界面/日志里。
String maskCardIdValue(String value) {
  if (value.length <= 10) {
    return value.isEmpty ? '（空）' : '${value.substring(0, 1)}…';
  }
  return '${value.substring(0, 6)}…${value.substring(value.length - 4)}';
}

/// 解析页面脚本返回的 JSON（容错：空/非法/引号包裹都返回空列表）。
List<CardIdHint> parseCardIdHints(String raw) {
  final String trimmed = raw.trim();
  if (trimmed.isEmpty || trimmed == 'null' || trimmed == '[]') {
    return const <CardIdHint>[];
  }
  // evaluateJavascript 可能返回带转义的字符串
  final String unquoted = trimmed.startsWith('"') && trimmed.endsWith('"')
      ? trimmed.substring(1, trimmed.length - 1).replaceAll(r'\"', '"').replaceAll(r'\\', r'\')
      : trimmed;
  try {
    final Object? decoded = jsonDecode(unquoted);
    if (decoded is! List) {
      return const <CardIdHint>[];
    }
    final List<CardIdHint> hints = <CardIdHint>[];
    final Set<String> seen = <String>{};
    for (final Object? item in decoded) {
      if (item is! Map) {
        continue;
      }
      final Object? value = item['v'];
      if (value is! String || value.isEmpty || seen.contains(value)) {
        continue;
      }
      if (!RegExp(r'^[0-9a-fA-F]{32}$').hasMatch(value)) {
        continue;
      }
      seen.add(value);
      hints.add(CardIdHint(value: value, source: '${item['s'] ?? 'scan'}'));
      if (hints.length >= 10) {
        break;
      }
    }
    return hints;
  } on FormatException {
    return const <CardIdHint>[];
  }
}

/// 找不到候选时给用户的提示。
const String kNoCardIdHintMessage = '未发现 32 位 hex；请确认当前页面是课表页（先进入「我的课表」再回来探测）。';
