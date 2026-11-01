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

/// 默认学期开始日（第 1 周周一）：**2026-09-07**（owner 已验证）。
///
/// 验算：按此锚点，第 1 周 = 09-07 ~ 09-13、第 8 周 = 10-26 ~ 11-01（owner 确认
/// 11-01 是第 8 周最后一天）、第 15 周 = 12-14 ~ 12-20（即抓包样本那一周）。
///
/// **可配置**：界面会显示这个值并允许用户修改，改完持久化到 `terms.starts_on`；
/// 这里只是"没有用户确认时的默认值"，不是不可变的硬编码。
const String kDefaultTermStart = '2026-09-07';

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

/// 默认学期开始日（`DateTime` 形式）。
DateTime get defaultTermStart => parseTermStart(kDefaultTermStart) ?? DateTime(2026, 9, 7);
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
  });

  final String cardId;
  final String years;
  final int? weekcount;

  factory ScheduleTemplate.fromMessage(Map<String, dynamic> message) => ScheduleTemplate(
        cardId: (message['card_id'] as String?) ?? '',
        years: (message['years'] as String?) ?? '',
        weekcount: _asInt(message['weekcount']),
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
    var url = parseUrl(rawUrl);
    if (!url || url.pathname.indexOf(API) < 0) { return; }
    var cardId = url.searchParams.get('cardId') || '';
    var years = url.searchParams.get('years') || '';
    if (!cardId || !years) { return; }
    if (TEMPLATE && TEMPLATE.cardId === cardId && TEMPLATE.years === years) { return; }
    TEMPLATE = { cardId: cardId, years: years, weekcount: 20, weekcountFromApi: false };
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

  window.__yunongFetchSchedule = function () {
    if (!TEMPLATE) {
      post({ type: 'error', code: 'CARD_NOT_FOUND', message: '没捕获到课表接口请求：请先在门户里打开一次课表卡片' });
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
