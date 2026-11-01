// 门户课表接口（`queryAWeekSchedule`）的端侧取数（W6 新形态）。
//
// 真实形态：门户是 React 单页，课表来自
//   GET /api/uppcard/kbsz/queryAWeekSchedule?years=<学期>&results=<周>&cardId=<卡片>&_t=...
// `cardId` 每人不同、`years` 是当前学期，**都不能写死**：注入脚本 hook
// `fetch`/`XMLHttpRequest`，捕获一次真实请求当模板，然后按模板批量拉 1..weekcount 周。
//
// 合并（相邻节次、周集合压缩）在 Rust 侧 `crate::schedule` 里做，这里只负责"取数 + 原样回传"。
//
// 失败姿势：
// - 401 / 被踢回登录页（返回 HTML 而不是 JSON）→ `auth_required`，回到登录流程；
// - 一次都没捕获到请求 → `CARD_NOT_FOUND`；
// - 部分周失败 → 照常回传成功的周，`missing_weeks` 如实上报（不整体失败）。

import 'dart:convert';

/// 接口路径（用于脚本匹配；改版时只改这里）。
const String kScheduleApiPath = '/api/uppcard/kbsz/queryAWeekSchedule';

/// 人工验证过的学期锚点（**仅作建议值，不是真值**），来源：云南农业大学
/// 2026—2027 学年校历。
///
/// 只放"校历上写明、且验算是周一"的学期；其它学期必须由用户确认。
/// **不要**把已知学期的值套用到别的学期：一学年有 **3** 个学期
/// （`2026-2027-1` 秋季 / `-2` 春季 / `-3` 暑期短学期），学期之间没有可推导的关系。
const Map<String, String> kSeededTermStarts = <String, String>{
  // 第一学期：在校生 9 月 7 日（周一）开始上课；16 周教学 + 2 周机动 + 1 周考试。
  '2026-2027-1': '2026-09-07',
  // 第二学期：3 月 1 日（周一）开始上课；16 周教学 + 1 周机动 + 1 周考试。
  '2026-2027-2': '2027-03-01',
  // 第三学期（暑期短学期）：7 月 5 日（周一）— 7 月 16 日，仅 2 周。
  '2026-2027-3': '2027-07-05',
};

/// 某学期的建议开始日。
///
/// 返回 `null` 表示"该学期没有人工验证过的锚点"，调用方必须要求用户确认，
/// 并且在确认前**不要显示任何具体日期**（只显示第几周）。
DateTime? suggestedTermStart(String term) =>
    parseTermStart(kSeededTermStarts[term] ?? '');

/// 取某天所在周的周一（本地时区，忽略时分秒）。
DateTime mondayOf(DateTime day) {
  final date = DateTime(day.year, day.month, day.day);
  return date.subtract(Duration(days: date.weekday - DateTime.monday));
}

/// 第 `week` 周的第一天（`week` 为 1 基）。
DateTime? weekStart({required DateTime termStart, required int week}) {
  if (week < 1) {
    return null;
  }
  final start = mondayOf(termStart);
  return start.add(Duration(days: (week - 1) * 7));
}

/// 第 `week` 周的最后一天。
DateTime? weekEnd({required DateTime termStart, required int week}) {
  final start = weekStart(termStart: termStart, week: week);
  return start?.add(const Duration(days: 6));
}

/// 今天落在第几周：`floor((today - startMonday) / 7) + 1`；开学前返回 `null`。
int? currentWeek({required DateTime termStart, required DateTime today}) {
  final start = mondayOf(termStart);
  final date = DateTime(today.year, today.month, today.day);
  final days = date.difference(start).inDays;
  if (days < 0) {
    return null;
  }
  return days ~/ 7 + 1;
}

/// `YYYY-MM-DD`（端侧 FFI 只接受这个形状）。
String formatIsoDate(DateTime date) {
  final month = date.month.toString().padLeft(2, '0');
  final day = date.day.toString().padLeft(2, '0');
  return '${date.year}-$month-$day';
}

/// `MM-DD` 短格式（只给 UI 显示周区间用）。
String formatShortDate(DateTime date) {
  final month = date.month.toString().padLeft(2, '0');
  final day = date.day.toString().padLeft(2, '0');
  return '$month-$day';
}

/// 第 `week` 周的区间标签，如 `09-07 ~ 09-13`。
String weekRangeLabel({required DateTime termStart, required int week}) {
  final start = weekStart(termStart: termStart, week: week);
  final end = weekEnd(termStart: termStart, week: week);
  if (start == null || end == null) {
    return '';
  }
  return '${formatShortDate(start)} ~ ${formatShortDate(end)}';
}

/// 解析用户确认过的开学日；坏值返回 `null`（调用方退回默认值）。
DateTime? parseTermStart(String value) {
  if (value.isEmpty) {
    return null;
  }
  final parsed = DateTime.tryParse(value);
  return parsed == null ? null : mondayOf(parsed);
}

/// 已废弃：**不再存在"全局默认学期开始日"**。
///
/// 保留此注释是为了防止有人重新引入全局常量：学期锚点必须按学期存放，
/// 见 [kSeededTermStarts] 与 [suggestedTermStart]。
/// 最多拉 20 周（接口 `weekcount` 上限的约定），并发 4，总超时 20s。
const int kMaxWeeks = 20;
const int kFetchConcurrency = 4;
const int kFetchTimeoutMs = 20000;

/// 捕获到的请求模板（cardId / years / weekcount 都是运行时拿的）。
class ScheduleTemplate {
  const ScheduleTemplate({
    required this.cardId,
    required this.years,
    this.weekcount,
    this.cardIdSource,
  });

  final String cardId;
  final String years;
  final int? weekcount;

  /// cardId 从哪来的（xhr / data-cardid / dom-id / initial-state / inline-script）
  final String? cardIdSource;

  factory ScheduleTemplate.fromMessage(Map<String, dynamic> message) => ScheduleTemplate(
        cardId: (message['card_id'] as String?) ?? '',
        years: (message['years'] as String?) ?? '',
        weekcount: _asInt(message['weekcount']),
        cardIdSource: message['card_id_source'] as String?,
      );

  bool get isValid => cardId.isNotEmpty && years.isNotEmpty;
}

/// 批量取数的结果（原样交给 Rust 合并）。
class ScheduleApiResult {
  const ScheduleApiResult({
    required this.weeks,
    required this.missingWeeks,
    this.weekcount,
    this.weekcountFallback = false,
    this.cardId,
    this.years,
  });

  /// `[{"week":1,"data":{...portal data...}}, ...]`
  final List<Map<String, dynamic>> weeks;

  /// 拉取失败的周号
  final List<int> missingWeeks;

  final int? weekcount;

  /// `weekcount` 是否为兜底值（接口没给，按 20 处理）
  final bool weekcountFallback;

  final String? cardId;
  final String? years;

  bool get isPartial => missingWeeks.isNotEmpty;

  int get weekCountLoaded => weeks.length;

  /// 交给 `yunong_import_schedule_json` 的 JSON。
  String toImportJson({required String term}) => jsonEncode(<String, dynamic>{
        'term': term,
        'weeks': weeks,
        'missing_weeks': missingWeeks,
        'weekcount': weekcount,
        'weekcount_fallback': weekcountFallback,
        'card_id': cardId,
        'years': years,
      });

  factory ScheduleApiResult.fromMessage(Map<String, dynamic> message) {
    final weeks = <Map<String, dynamic>>[];
    final rawWeeks = message['weeks'];
    if (rawWeeks is List) {
      for (final item in rawWeeks) {
        if (item is! Map) {
          continue;
        }
        final week = _asInt(item['week']);
        final data = item['data'];
        if (week == null || data is! Map<String, dynamic>) {
          continue;
        }
        weeks.add(<String, dynamic>{'week': week, 'data': data});
      }
    }
    final missing = <int>[];
    final rawMissing = message['missing_weeks'];
    if (rawMissing is List) {
      for (final item in rawMissing) {
        final week = _asInt(item);
        if (week != null) {
          missing.add(week);
        }
      }
    }
    return ScheduleApiResult(
      weeks: weeks,
      missingWeeks: missing.toList()..sort(),
      weekcount: _asInt(message['weekcount']),
      weekcountFallback: message['weekcount_fallback'] == true,
      cardId: message['card_id'] as String?,
      years: message['years'] as String?,
    );
  }
}
int? _asInt(Object? value) {
  if (value is int) {
    return value;
  }
  if (value is String) {
    return int.tryParse(value.trim());
  }
  if (value is num) {
    return value.toInt();
  }
  return null;
}

/// 注入到门户页面的脚本：hook 请求拿到模板 → 批量拉周 → 回传。
///
/// 隐私：只回传课表数据（课程名/教室/教师/节次），**不读 Cookie、不回传任何请求头**。
const String kPortalScheduleScript = r'''
(function () {
  if (window.__yunongPortal) { return; }
  window.__yunongPortal = true;

  var API = '/api/uppcard/kbsz/queryAWeekSchedule';
  var TEMPLATE = null;
  var running = false;
  // 诊断用：owner/我们据此判断卡在哪一步（不含任何凭据）
  var DIAG = { xhrSeen: 0, cardIdSource: '', cardIdPrefix: '', autoTried: 0 };
  var TIMEOUT_MS = 20000;
  var timeoutPosted = false;

  function post(payload) {
    try {
      if (!window.flutter_inappwebview) { return false; }
      window.flutter_inappwebview.callHandler('yunongBridge', JSON.stringify(payload));
      return true;
    } catch (e) { return false; }
  }

  function parseUrl(raw) {
    try { return new URL(raw, location.href); } catch (e) { return null; }
  }

  function capture(rawUrl) {
    DIAG.xhrSeen += 1;
    var url = parseUrl(rawUrl);
    if (!url || url.pathname.indexOf(API) < 0) { return; }
    var cardId = url.searchParams.get('cardId') || '';
    var years = url.searchParams.get('years') || '';
    if (!cardId || !years) { return; }
    if (TEMPLATE && TEMPLATE.cardId === cardId && TEMPLATE.years === years) { return; }
    TEMPLATE = { cardId: cardId, years: years, weekcount: 20, weekcountFromApi: false };
    DIAG.cardIdPrefix = cardId.slice(0, 8);
    DIAG.cardIdSource = 'xhr';
    post({ type: 'schedule_template', card_id: cardId, years: years, weekcount: TEMPLATE.weekcount });
    // 捕获到模板就自动批量拉，用户不需要再点任何按钮
    setTimeout(function () { window.__yunongFetchSchedule(); }, 300);
  }

  // ① hook fetch
  var rawFetch = window.fetch;
  if (rawFetch) {
    window.fetch = function (input) {
      try { capture(typeof input === 'string' ? input : (input && input.url)); } catch (e) {}
      return rawFetch.apply(this, arguments);
    };
  }
  // ② hook XHR（Ant Design 的旧组件仍可能用 XHR）
  if (window.XMLHttpRequest) {
    var rawOpen = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function (method, url) {
      try { capture(url); } catch (e) {}
      return rawOpen.apply(this, arguments);
    };
  }

  function getJson(url) {
    return rawFetch.call(window, url, {
      credentials: 'include',
      headers: { 'Accept': 'application/json, text/plain, */*' }
    }).then(function (res) {
      if (res.status === 401 || res.status === 403) {
        var err = new Error('AUTH');
        err.code = 'AUTH_REQUIRED';
        throw err;
      }
      return res.text().then(function (text) {
        var trimmed = (text || '').trim();
        // 被踢回登录页时返回的是 HTML：这不是"这学期没课"，是登录态失效
        if (!trimmed || trimmed.charAt(0) !== '{') {
          var err = new Error('NON_JSON');
          err.code = 'AUTH_REQUIRED';
          throw err;
        }
        return JSON.parse(trimmed);
      });
    });
  }

  function weekUrl(week) {
    return API + '?years=' + encodeURIComponent(TEMPLATE.years)
      + '&results=' + week
      + '&cardId=' + encodeURIComponent(TEMPLATE.cardId)
      + '&_t=' + Date.now();
  }

  function safePath() {
    try { return location.pathname || ''; } catch (e) { return ''; }
  }

  window.__yunongDiagnostics = function () {
    var title = '';
    try { title = document.title || ''; } catch (e) {}
    return JSON.stringify({
      path: safePath(),
      host: (location.hostname || ''),
      title: title,
      captured: !!TEMPLATE,
      xhrSeen: DIAG.xhrSeen,
      cardIdPrefix: DIAG.cardIdPrefix,
      cardIdSource: DIAG.cardIdSource,
      autoTried: DIAG.autoTried
    });
  };

  function postCaptureTimeout(reason) {
    if (timeoutPosted) { return; }
    timeoutPosted = true;
    post({
      type: 'capture_timeout',
      reason: reason || 'no_template',
      diagnostics: JSON.parse(window.__yunongDiagnostics())
    });
  }

  // 20s 还没捕获到任何课表请求 → 明确告诉用户该做什么，绝不静默挂起
  setTimeout(function () {
    if (!TEMPLATE) { postCaptureTimeout('no_template_20s'); }
  }, TIMEOUT_MS);
  // ---- cardId 兜底：捕获不到真实请求时，从页面里读 ----
  //
  // 已知样例：卡片 DOM id 形如 `timeTableWeekCard_d907dd70`（hash 可能就是 cardId），
  // 门户是 React 单页，初始状态可能挂在 `__INITIAL_STATE__` / `__NUXT__` 或内联 script 里。
  function deepFindCardId(value, depth) {
    if (!value || depth > 6) { return ''; }
    if (Array.isArray(value)) {
      for (var i = 0; i < value.length && i < 80; i++) {
        var found = deepFindCardId(value[i], depth + 1);
        if (found) { return found; }
      }
      return '';
    }
    if (typeof value !== 'object') { return ''; }
    for (var key in value) {
      if (!Object.prototype.hasOwnProperty.call(value, key)) { continue; }
      if (key.toLowerCase() === 'cardid'
          && typeof value[key] === 'string' && value[key].length >= 8) {
        return value[key];
      }
      var nested = deepFindCardId(value[key], depth + 1);
      if (nested) { return nested; }
    }
    return '';
  }

  function termFromDom() {
    try {
      var html = document.documentElement.innerHTML || '';
      var match = html.match(/(20\d{2}-20\d{2}-[12])/);
      if (match) { return match[1]; }
    } catch (e) {}
    try {
      var nodes = document.querySelectorAll('.ant-select-selection-item, .ant-select span');
      for (var i = 0; i < nodes.length; i++) {
        var text = (nodes[i].textContent || '').trim();
        if (/^20\d{2}-20\d{2}-[12]$/.test(text)) { return text; }
      }
    } catch (e) {}
    return '';
  }

  function cardIdFromPage() {
    try {
      var el = document.querySelector('[data-cardid]');
      if (el) {
        var attrValue = el.getAttribute('data-cardid') || '';
        if (attrValue.length >= 8) { return { cardId: attrValue, source: 'data-cardid' }; }
      }
    } catch (e) {}
    try {
      var node = document.querySelector('[id*="timeTable"], [id*="kbsz"], [id*="Card"]');
      if (node) {
        var attr = node.getAttribute('data-id') || node.getAttribute('data-card-id') || '';
        if (attr.length >= 8) { return { cardId: attr, source: 'dom-attr' }; }
        var id = node.id || '';
        var hash = id.indexOf('_') >= 0 ? id.split('_').pop() : id;
        if (hash && hash.length >= 8) { return { cardId: hash, source: 'dom-id' }; }
      }
    } catch (e) {}
    var states = [];
    try { states.push(window.__INITIAL_STATE__); } catch (e) {}
    try { states.push(window.__NUXT__); } catch (e) {}
    try { states.push(window.__DATA__); } catch (e) {}
    for (var i = 0; i < states.length; i++) {
      var found = deepFindCardId(states[i], 0);
      if (found) { return { cardId: found, source: 'initial-state' }; }
    }
    try {
      var html = document.documentElement.innerHTML || '';
      var re = /"cardId"\s*:\s*"([0-9a-zA-Z_-]{8,})"/g;
      var match = re.exec(html);
      while (match) {
        var around = html.slice(Math.max(0, match.index - 200), match.index + 200);
        if (around.indexOf('课表') >= 0 || around.indexOf('kbsz') >= 0
            || around.indexOf('timeTable') >= 0 || around.indexOf('schedule') >= 0) {
          return { cardId: match[1], source: 'inline-script' };
        }
        match = re.exec(html);
      }
    } catch (e) {}
    return null;
  }

  function tryFallbackTemplate() {
    if (TEMPLATE) { return true; }
    var found = cardIdFromPage();
    if (!found) { return false; }
    var years = termFromDom();
    if (!years) { return false; }
    TEMPLATE = { cardId: found.cardId, years: years, weekcount: 20, weekcountFromApi: false };
    DIAG.cardIdPrefix = found.cardId.slice(0, 8);
    DIAG.cardIdSource = found.source;
    post({
      type: 'schedule_template',
      card_id: found.cardId,
      years: years,
      weekcount: 20,
      card_id_source: found.source
    });
    setTimeout(function () { window.__yunongFetchSchedule(); }, 300);
    return true;
  }

  // ---- 自动触发：滚动/点击含"课表"的卡片，切换周选择器 ----
  function autoTrigger() {
    DIAG.autoTried += 1;
    try {
      var nodes = document.querySelectorAll('div, section, li, a, button, span');
      for (var i = 0; i < nodes.length && i < 4000; i++) {
        var text = (nodes[i].textContent || '').trim();
        if (text.length > 0 && text.length <= 40 && text.indexOf('课表') >= 0) {
          try { nodes[i].scrollIntoView({ block: 'center' }); } catch (e) {}
          try { nodes[i].click(); } catch (e) {}
          break;
        }
      }
    } catch (e) {}
    try {
      var selects = document.querySelectorAll('.ant-select-selection, .ant-select, select');
      if (selects.length) { selects[0].click(); }
    } catch (e) {}
    tryFallbackTemplate();
    try { window.__yunongFetchSchedule(); } catch (e) {}
  }

  setTimeout(function () { if (!TEMPLATE) { autoTrigger(); } }, 3000);
  setTimeout(function () { if (!TEMPLATE) { autoTrigger(); } }, 8000);
  window.__yunongFetchSchedule = function () {
    if (!TEMPLATE) {
      post({
        type: 'capture_timeout',
        reason: 'manual_grab_without_template',
        diagnostics: JSON.parse(window.__yunongDiagnostics())
      });
      return;
    }
    if (running) { return; }
    running = true;

    var total = Math.min(parseInt(TEMPLATE.weekcount, 10) || 20, 20);
    var pending = [];
    for (var w = 1; w <= total; w++) { pending.push(w); }
    var weeks = [];
    var missing = [];
    var active = 0;
    var auth = false;
    var finished = false;
    var deadline = Date.now() + 20000;

    function finish() {
      if (finished) { return; }
      finished = true;
      running = false;
      if (auth) {
        post({ type: 'auth_required', message: '登录态已失效，请重新登录' });
        return;
      }
      post({
        type: 'schedule_api',
        weeks: weeks,
        missing_weeks: missing.sort(function (a, b) { return a - b; }),
        weekcount: total,
        weekcount_fallback: !TEMPLATE.weekcountFromApi,
        years: TEMPLATE.years,
        card_id: TEMPLATE.cardId
      });
    }

    function pump() {
      if (finished) { return; }
      if (auth) { finish(); return; }
      if (!pending.length && active === 0) { finish(); return; }
      if (Date.now() > deadline) {
        missing = missing.concat(pending);
        pending = [];
        finish();
        return;
      }
      while (pending.length && active < 4) {
        (function (week) {
          active += 1;
          getJson(weekUrl(week)).then(function (body) {
            var data = body && body.data ? body.data : null;
            if (data && data.resultsJsonArr) {
              weeks.push({ week: week, data: data });
              var wc = parseInt(data.weekcount, 10);
              if (wc && !isNaN(wc)) { TEMPLATE.weekcount = wc; TEMPLATE.weekcountFromApi = true; }
            } else {
              missing.push(week);
            }
          }).catch(function (err) {
            if (err && err.code === 'AUTH_REQUIRED') { auth = true; }
            else { missing.push(week); }
          }).then(function () {
            active -= 1;
            pump();
          });
        })(pending.shift());
      }
    }

    pump();
  };

  // 手动重试入口（UI 上的"抓取当前页"会调用它）
  window.__yunongGrab = function () { window.__yunongFetchSchedule(); };
})();
''';
