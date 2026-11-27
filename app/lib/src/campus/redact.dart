// 诊断脱敏的共享掩码规则（security R7 M2）。
//
// 为什么要单独一份：完整 cardId 曾同时出现在 `capture_log`（明文）与 `capture_cache`
// （前 6…后 4）两套口径里。现在所有出口（日志 / 复制诊断 / 缓存展示 / URL）都用这里的
// `maskIdentifier`，避免再次分叉。

import '../l10n/strings.dart';

/// 稳定的标识掩码：保留头部 [keepHead] 位 + `…` + 尾部 [keepTail] 位。
///
/// 默认与 `CaptureCacheEntry.maskedCardId` 完全一致（前 6 … 后 4）：
/// 既去掉可直接用于查询的完整值，又保留足够前缀/后缀供排障比对。
String maskIdentifier(
  String value, {
  int keepHead = 6,
  int keepTail = 4,
  String ellipsis = '…',
}) {
  final String text = value.trim();
  if (text.isEmpty) {
    return text;
  }
  if (text.length <= keepHead + keepTail) {
    return CampusS.redacted;
  }
  final String head = keepHead <= 0 ? '' : text.substring(0, keepHead);
  final String tail = keepTail <= 0 ? '' : text.substring(text.length - keepTail);
  return '$head$ellipsis$tail';
}

/// 学号掩码：不保留前缀（能定位到人），只保留后 4 位便于核对。
String maskStudentId(String digits) {
  final String text = digits.trim();
  if (text.length <= 4) {
    return CampusS.redacted;
  }
  return '****${text.substring(text.length - 4)}';
}
