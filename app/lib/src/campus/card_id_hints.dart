// 【t142】cardId 候选的 Dart 侧解析与脱敏（纯数据，不发请求、不读 Cookie）。
//
// 页面脚本 `kCardIdProbeInstallScript` 返回 `JSON.stringify([{v, s}, …])`：
//   v = 32 位 hex 的完整值（页面内标识，不是凭据）
//   s = 来源（`window.xxx` / `xhr:/api/...` / `html` / `localStorage.xxx` / `sessionStorage.xxx`）

import 'dart:async';
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

/// 【t143】不在课表页时脚本返回的错误码。
const String kNotOnSchedulePageError = 'NOT_ON_SCHEDULE_PAGE';

/// 【t143】`evaluateJavascript(source:)` 的直接返回值（最后一个表达式的值）。
typedef ScriptEvaluator = Future<Object?> Function(String source);

/// 探测结果：要么有候选，要么有一条**能显示给用户**的错误。
class CardIdProbeOutcome {
  const CardIdProbeOutcome({this.hints = const <CardIdHint>[], this.error});

  final List<CardIdHint> hints;
  final String? error;

  bool get ok => error == null;

  String get message {
    if (error != null) {
      return error!;
    }
    if (hints.isEmpty) {
      return kNoCardIdHintMessage;
    }
    return '发现 ${hints.length} 个候选';
  }
}

/// 【t143】自包含探测表达式：**直接 return JSON 字符串**，不依赖全局变量、不走 JS→Dart 桥。
///
/// 旧实现是"安装 `window.__yunongProbeCardIdsRaw` + 桥回传"，问题在于：页面导航后
/// 全局/桥可能失效，回调不触发，Dart 侧就永远等不到结果（按钮一直是灰的）。
const String kCardIdProbeExpression = r'''
(() => {
  const onSchedulePage = (() => {
    try {
      const href = String(location.href || '');
      if (/queryAWeekSchedule|kbsz|\/#\/index5/.test(href)) { return true; }
      const text = String(document.body ? document.body.innerText : '').slice(0, 4000);
      return /(我的课表|课表|周次|学期)/.test(text) || !!document.querySelector('[data-cardid]');
    } catch (e) { return false; }
  })();
  if (!onSchedulePage) { return JSON.stringify({ error: 'NOT_ON_SCHEDULE_PAGE' }); }
  try {
    const HEX = /^[0-9a-fA-F]{32}$/;
    const HEX_ANY = /[0-9a-fA-F]{32}/g;
    const out = [];
    const seen = {};
    const push = (v, s) => {
      if (!v || !HEX.test(v) || seen[v]) { return; }
      seen[v] = 1;
      out.push({ v, s });
    };
    const scanText = (text, tag) => {
      if (!text || typeof text !== 'string') { return; }
      const m = text.match(HEX_ANY);
      if (!m) { return; }
      for (let i = 0; i < m.length && i < 6; i++) { push(m[i], tag); }
    };
    try {
      for (const name of Object.keys(window)) {
        let val;
        try { val = window[name]; } catch (e) { continue; }
        if (typeof val === 'string') { push(val, 'window.' + name); continue; }
        if (!val || typeof val !== 'object') { continue; }
        try {
          for (const key of Object.keys(val).slice(0, 40)) {
            const sv = val[key];
            if (typeof sv === 'string') { push(sv, 'window.' + name + '.' + key); }
          }
        } catch (e2) {}
      }
    } catch (e) {}
    try {
      const log = window.__yunongXhrLog || [];
      for (const item of log) {
        scanText(item && item.url, 'xhr:' + String(item && item.url || '').slice(0, 60));
        scanText(item && item.body, 'xhrbody:' + String(item && item.url || '').slice(0, 40));
      }
    } catch (e) {}
    try { scanText(document.documentElement.outerHTML || '', 'html'); } catch (e) {}
    try {
      for (const label of ['localStorage', 'sessionStorage']) {
        const store = window[label];
        if (!store) { continue; }
        for (let i = 0; i < store.length; i++) {
          const key = store.key(i);
          let raw = '';
          try { raw = store.getItem(key) || ''; } catch (e2) {}
          push(raw, label + '.' + key);
          scanText(raw, label + '.' + key);
        }
      }
    } catch (e) {}
    return JSON.stringify(out.slice(0, 12));
  } catch (e) {
    return JSON.stringify({ error: 'JS_ERROR: ' + String(e) });
  }
})()
''';

/// 解析 `evaluateJavascript` 的返回值（null / 非字符串 / 非法 JSON / 页面不对 都显式判错）。
CardIdProbeOutcome parseCardIdProbeReturn(Object? raw) {
  if (raw == null) {
    return const CardIdProbeOutcome(error: '探测失败：evaluateJavascript 返回 null（页面可能还没就绪）');
  }
  if (raw is! String) {
    return CardIdProbeOutcome(
      error: '探测失败：evaluateJavascript 返回了 ${raw.runtimeType}（不是字符串，可能是 Promise/对象）',
    );
  }
  final String text = raw.trim();
  if (text.isEmpty) {
    return const CardIdProbeOutcome(error: '探测失败：evaluateJavascript 返回空字符串');
  }
  Object? decoded;
  try {
    decoded = jsonDecode(text);
  } on FormatException catch (failure) {
    return CardIdProbeOutcome(error: '探测失败：返回的不是合法 JSON（${failure.message}）');
  }
  if (decoded is Map && decoded['error'] != null) {
    final String code = '${decoded['error']}';
    if (code.contains(kNotOnSchedulePageError)) {
      return const CardIdProbeOutcome(error: '当前不在课表页：请先进入「我的课表」并等页面加载完，再回来探测');
    }
    return CardIdProbeOutcome(error: '探测失败：页面脚本报错（$code）');
  }
  if (decoded is! List) {
    return CardIdProbeOutcome(error: '探测失败：返回值不是候选数组（${decoded.runtimeType}）');
  }
  return CardIdProbeOutcome(hints: parseCardIdHints(text));
}

/// 【t143】跑一次探测：`evaluateJavascript` 直接取值 + 5s 超时；**任何分支都返回结果**（不抛）。
Future<CardIdProbeOutcome> runCardIdProbe({
  required ScriptEvaluator evaluate,
  Duration timeout = const Duration(seconds: 5),
  String expression = kCardIdProbeExpression,
}) async {
  final Completer<Object?> completer = Completer<Object?>();
  final Timer timer = Timer(timeout, () {
    if (!completer.isCompleted) {
      completer.completeError(TimeoutException('探测超时 ${timeout.inSeconds}s'));
    }
  });
  unawaited(evaluate(expression).then((Object? value) {
    if (!completer.isCompleted) {
      completer.complete(value);
    }
  }, onError: (Object error) {
    if (!completer.isCompleted) {
      completer.completeError(error);
    }
  }));

  try {
    final Object? raw = await completer.future;
    return parseCardIdProbeReturn(raw);
  } on TimeoutException {
    return CardIdProbeOutcome(error: '探测超时（${timeout.inSeconds}s，可能页面未就绪），可重试');
  } on Object catch (failure) {
    return CardIdProbeOutcome(error: '探测失败：${failure.runtimeType}（$failure）');
  } finally {
    timer.cancel();
  }
}
