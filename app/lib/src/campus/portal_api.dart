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

/// 展开周次字符串（`1-16` / `1-8,10-16` / `1,3,5`）为周号集合。
/// 解析不出来就返回空集合（不做猜测）。
Set<int> expandWeeks(String spec) {
  final weeks = <int>{};
  for (final part in spec.split(',')) {
    final text = part.trim();
    if (text.isEmpty) {
      continue;
    }
    final dash = text.indexOf('-');
    if (dash > 0) {
      final start = int.tryParse(text.substring(0, dash).trim());
      final end = int.tryParse(text.substring(dash + 1).trim());
      if (start != null && end != null && start <= end && end - start <= 60) {
        for (var week = start; week <= end; week++) {
          weeks.add(week);
        }
      }
      continue;
    }
    final single = int.tryParse(text);
    if (single != null && single > 0) {
      weeks.add(single);
    }
  }
  return weeks;
}

/// 这门课在第 `week` 周是否有课。
bool occursInWeek(String weeksSpec, int week) {
  if (week < 1) {
    return false;
  }
  return expandWeeks(weeksSpec).contains(week);
}
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
/// 当前是不是 SSO 登录页（**登录等待阶段**：不扫描、不探测、不计抓取超时）。
///
/// 判据与脚本一致：path 含 `/lyuapServer/login`，或 host 是 `ssoo.ynau.edu.cn` 且 path 含 login。
bool isSsoLoginPage(String host, String path) {
  final lowerPath = path.toLowerCase();
  if (lowerPath.contains('/lyuapserver/login')) {
    return true;
  }
  return host.contains('ssoo.ynau.edu.cn') && lowerPath.contains('login');
}
/// 一个候选 cardId（32 位 hex）+ 它在 HTML 里的上下文（用于排序与诊断，不含凭据）。
class CardIdCandidate {
  const CardIdCandidate({required this.value, required this.context, required this.order});

  final String value;

  /// 命中处前后各 40 字符（排序/诊断用；不打印整页 HTML）
  final String context;

  /// 在 HTML 里出现的顺序（越小越靠前）
  final int order;

  /// 上下文里是否带课表相关线索（排序优先）
  bool get looksLikeTimetable {
    final text = context.toLowerCase();
    return context.contains('课表') ||
        text.contains('kbsz') ||
        text.contains('timetable') ||
        text.contains('schedule');
  }
}

/// 从整页 HTML 里抽 32 位 hex 候选：**按出现顺序去重**，课表相关线索排前。
///
/// 位置不可靠（owner 实测 18 个命中里有 6 个是重复的，真实 cardId 排第 14），
/// 所以这里只负责"候选 + 优先级"，命中与否必须靠 [`selectCardIdByProbing`] 探测。
List<CardIdCandidate> extractCardIdCandidates(String html, {int max = 12}) {
  if (html.isEmpty || max <= 0) {
    return const <CardIdCandidate>[];
  }
  final pattern = RegExp(r'[0-9a-f]{32}');
  final seen = <String>{};
  final ordered = <CardIdCandidate>[];
  for (final match in pattern.allMatches(html)) {
    final value = match.group(0)!;
    if (!seen.add(value)) {
      continue;
    }
    final start = match.start - 40 < 0 ? 0 : match.start - 40;
    final end = match.end + 40 > html.length ? html.length : match.end + 40;
    ordered.add(CardIdCandidate(
      value: value,
      context: html.substring(start, end),
      order: ordered.length,
    ));
  }
  final sorted = <CardIdCandidate>[
    ...ordered.where((CardIdCandidate item) => item.looksLikeTimetable),
    ...ordered.where((CardIdCandidate item) => !item.looksLikeTimetable),
  ];
  return sorted.take(max).toList(growable: false);
}

/// 探测命中判据：`data.resultsJsonArr` 是一个**行数组**（门户固定 7 天）。
bool isScheduleProbeHit(Object? decoded) {
  if (decoded is! Map) {
    return false;
  }
  final data = decoded['data'];
  if (data is! Map) {
    return false;
  }
  final rows = data['resultsJsonArr'];
  if (rows is! List || rows.isEmpty) {
    return false;
  }
  return true;
}

/// 依次探测候选，命中即停（最多 [maxAttempts] 次）。
///
/// `probe` 返回 true 表示命中；异常/非 200 都按"不命中"处理（不中断整体流程）。
Future<CardIdCandidate?> selectCardIdByProbing(
  List<CardIdCandidate> candidates,
  Future<bool> Function(CardIdCandidate candidate) probe, {
  int maxAttempts = 12,
}) async {
  var attempts = 0;
  for (final candidate in candidates) {
    if (attempts >= maxAttempts) {
      break;
    }
    attempts += 1;
    try {
      if (await probe(candidate)) {
        return candidate;
      }
    } on Object {
      // 单个候选失败不影响后续候选（真实页面里大量 hash 是无关业务 id）
      continue;
    }
  }
  return null;
}
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
  var DIAG = {
    xhrSeen: 0, cardIdSource: '', cardIdPrefix: '', autoTried: 0,
    hookInstalled: true, candidates: 0, years: '', probeFailed: false, probeResults: []
  };
  // **没有任何"从页面打开算起"的绝对超时**：只有"无进展兜底"（见 watchdog）
  var LAST_PROGRESS = { at: Date.now(), what: 'start' };
  var NO_PROGRESS_MS = 15000;
  var timeoutPosted = false;

  function progress(what) {
    LAST_PROGRESS = { at: Date.now(), what: what };
    timeoutPosted = false;
  }

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
    progress('hook 捕获到课表请求');
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
      stage: STAGE,
      cardDom: !!timetableCardNode(),
      scans: SCANS,
      candidates: CANDIDATE_ORDER.length,
      sinceStageEnter: Math.round((Date.now() - STAGE_ENTERED_AT) / 1000),
      lastProgress: LAST_PROGRESS.what + '（距今 ' + Math.round((Date.now() - LAST_PROGRESS.at) / 1000) + 's）',
      path: safePath(),
      host: (location.hostname || ''),
      title: title,
      captured: !!TEMPLATE,
      xhrSeen: DIAG.xhrSeen,
      cardIdPrefix: DIAG.cardIdPrefix,
      cardIdSource: DIAG.cardIdSource,
      autoTried: DIAG.autoTried,
      hookInstalled: DIAG.hookInstalled,
      candidates: DIAG.candidates,
      years: DIAG.years || '',
      probeFailed: DIAG.probeFailed,
      probeResults: DIAG.probeResults
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

  // 无进展兜底：只在"已离开登录页"且 15s 没有任何进展时才报，登录页永远不报
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

  // ---- 阶段状态机：登录前**绝不做**候选扫描/探测/抓取 ----
  //
  // owner 实测的 bug：还在 SSO 登录页输入账号时，20s 抓取超时就到了。
  // 现在登录页阶段只上报 stage=login，抓取计时/扫描/探测全部等到离开登录页才开始。
  var STAGE = 'login';
  var STAGE_WATCHER = null;

  function isLoginPage() {
    try {
      var host = location.hostname || '';
      var path = location.pathname || '';
      if (path.indexOf('/lyuapServer/login') >= 0) { return true; }
      if (host.indexOf('ssoo.ynau.edu.cn') >= 0 && path.indexOf('login') >= 0) { return true; }
      return false;
    } catch (e) {
      return false;
    }
  }

  function diagnosticsJson() {
    try { return JSON.parse(window.__yunongDiagnostics()); } catch (e) { return {}; }
  }

  function setStage(next) {
    if (STAGE === next) { return; }
    STAGE = next;
    post({ type: 'stage', stage: next, diagnostics: diagnosticsJson() });
  }

  // 每 1.5s 看一次：还在登录页就只保持 login 阶段；离开后切到 capture 并开始自动发现
  // 课表卡片的选择器（门户 React 页面；多个都试，命中任一即可）
  var CARD_SELECTORS = ['.timeTableWeekCard', '#courseWrapper', '.Courses-content', '[id*="timeTable"]'];

  function timetableCardNode() {
    for (var i = 0; i < CARD_SELECTORS.length; i++) {
      try {
        if (document.querySelector(CARD_SELECTORS[i])) { return CARD_SELECTORS[i]; }
      } catch (e) {}
    }
    return '';
  }

  // 阶段机（**事件驱动**，不是计时驱动）：
  //   login（登录页，什么都不做）→ waiting_card（等卡片 DOM 或 XHR）→ capture（扫候选→探测→拉周）
  function watchStage() {
    if (isLoginPage()) {
      setStage('login');
      return;
    }
    var card = timetableCardNode();
    if (card) {
      progress('检测到课表卡片 ' + card);
      setStage('capture');
      if (!TEMPLATE) { autoStart(); }
      return;
    }
    if (TEMPLATE) {
      setStage('capture');
      return;
    }
    // 已离开登录页但卡片还没渲染：等 MutationObserver / XHR，不算失败
    setStage('waiting_card');
  }

  function installCardObserver() {
    try {
      if (!window.MutationObserver || !document.body) { return; }
      var observer = new MutationObserver(function () { watchStage(); });
      observer.observe(document.body, { childList: true, subtree: true });
    } catch (e) {}
  }

  // 无进展兜底：登录页阶段永不报；其它阶段 15s 没进展才报（诊断里写最后进展）
  setInterval(function () {
    if (STAGE === 'login') { return; }
    if (Date.now() - LAST_PROGRESS.at > NO_PROGRESS_MS) {
      postCaptureTimeout('no_progress');
    }
  }, 1000);

  installCardObserver();
  setTimeout(watchStage, 600);
  setInterval(watchStage, 1500);
  // capture 阶段每 500ms 重扫：React 逐步渲染时候选会从 2 涨到 ~12
  setInterval(rescan, 500);
  // ---- 自动优先（owner 实测：hook 根本没挂上，不能等它）----
  //
  // 顺序：① 从页面读 cardId（DOM 属性 / 初始状态 / 内联 script）→ ② 32 位 hex 候选逐个点接口自验证
  // → ③ 不带 cardId 的自探测（服务端若忽略该参数直接返回本人课表，这条路最省事）。
  // 全自动，不需要用户点任何东西；都失败才交给 20s 超时给具体诊断。
  var AUTO_STARTED = false;

  // ---- 扫描范围：整页 outerHTML + 同源 iframe；持续重扫，候选集合累积 ----
  var CANDIDATE_ORDER = [];   // [{ value, context, prefers }]，按出现顺序
  var CANDIDATE_SEEN = {};    // value → true
  var PROBED = {};            // 已探测过的候选
  var SCANS = 0;
  var STAGE_ENTERED_AT = Date.now();

  function scanSources() {
    var sources = [];
    try { sources.push(document.documentElement.outerHTML || ''); } catch (e) {}
    try {
      var frames = document.querySelectorAll('iframe');
      for (var i = 0; i < frames.length; i++) {
        try {
          var doc = frames[i].contentDocument;   // 跨域会抛异常，忽略
          if (doc && doc.documentElement) {
            sources.push(doc.documentElement.outerHTML || '');
          }
        } catch (e) {}
      }
    } catch (e) {}
    return sources;
  }

  // 返回本次**新增**候选数（并并入 CANDIDATE_ORDER，保序去重）
  function scanCandidates() {
    SCANS += 1;
    var added = 0;
    var sources = scanSources();
    for (var s = 0; s < sources.length; s++) {
      var html = sources[s];
      if (!html) { continue; }
      var re = /[0-9a-f]{32}/g;
      var match = re.exec(html);
      while (match) {
        var value = match[0];
        if (!CANDIDATE_SEEN[value]) {
          CANDIDATE_SEEN[value] = true;
          var begin = match.index - 40 < 0 ? 0 : match.index - 40;
          var finish = match.index + 32 + 40 > html.length ? html.length : match.index + 32 + 40;
          var context = html.slice(begin, finish);
          var lower = context.toLowerCase();
          CANDIDATE_ORDER.push({
            value: value,
            context: context,
            prefers: context.indexOf('课表') >= 0 || lower.indexOf('kbsz') >= 0
              || lower.indexOf('timetable') >= 0 || lower.indexOf('schedule') >= 0
          });
          added += 1;
        }
        match = re.exec(html);
      }
    }
    return added;
  }

  function sortedCandidates() {
    var sorted = [];
    for (var i = 0; i < CANDIDATE_ORDER.length; i++) {
      if (CANDIDATE_ORDER[i].prefers) { sorted.push(CANDIDATE_ORDER[i]); }
    }
    for (var j = 0; j < CANDIDATE_ORDER.length; j++) {
      if (!CANDIDATE_ORDER[j].prefers) { sorted.push(CANDIDATE_ORDER[j]); }
    }
    return sorted;
  }
  function termGuess() {
    if (window.__yunongTerm) { return String(window.__yunongTerm); }
    return termFromDom();
  }

  function probeCandidate(cardId, years) {
    if (!cardId) { return Promise.resolve(false); }
    var week = window.__yunongCurrentWeek || 1;
    var url = API + '?years=' + encodeURIComponent(years) + '&results=' + week
      + (cardId ? '&cardId=' + encodeURIComponent(cardId) : '')
      + '&_t=' + Date.now();
    return rawFetch.call(window, url, {
      credentials: 'include',
      headers: { 'Accept': 'application/json, text/plain, */*' }
    }).then(function (res) {
      var prefix = cardId ? cardId.slice(0, 8) : '(none)';
      DIAG.probeResults.push(prefix + ':' + res.status);
      progress('探测 ' + prefix + ' → ' + res.status);
      if (res.status === 401 || res.status === 403) {
        var authErr = new Error('AUTH');
        authErr.code = 'AUTH_REQUIRED';
        throw authErr;
      }
      if (res.status < 200 || res.status >= 300) { return false; }
      return res.text().then(function (text) {
        var trimmed = (text || '').trim();
        if (!trimmed || trimmed.charAt(0) !== '{') { return false; }
        try {
          var body = JSON.parse(trimmed);
          var data = body && body.data ? body.data : null;
          return !!(data && data.resultsJsonArr && data.resultsJsonArr.length);
        } catch (e) { return false; }
      });
    }).catch(function (err) {
      DIAG.probeResults.push((cardId ? cardId.slice(0, 8) : '(none)') + ':ERR');
      if (err && err.code === 'AUTH_REQUIRED') { throw err; }
      return false;
    });
  }
  function adoptTemplate(cardId, source, years) {
    if (TEMPLATE) { return; }
    TEMPLATE = { cardId: cardId, years: years, weekcount: 20, weekcountFromApi: false };
    DIAG.cardIdPrefix = cardId ? cardId.slice(0, 8) : '';
    DIAG.cardIdSource = source;
    post({
      type: 'schedule_template',
      card_id: cardId,
      years: years,
      weekcount: 20,
      card_id_source: source
    });
    setTimeout(function () { window.__yunongFetchSchedule(); }, 200);
  }

  // 进入抓取阶段：**必须重置进展时间戳**（owner 实测 bug：阶段一进就报超时）
  function enterCapture() {
    if (STAGE === 'capture') { return; }
    STAGE = 'capture';
    STAGE_ENTERED_AT = Date.now();
    progress('进入抓取阶段');
    post({ type: 'stage', stage: 'capture', diagnostics: diagnosticsJson() });
    rescan();
  }

  // 持续重扫：只在 capture 阶段；每轮把新候选并入集合并逐个探测
  function rescan() {
    if (STAGE !== 'capture' || TEMPLATE) { return; }
    if (isLoginPage()) { setStage('login'); return; }
    var added = scanCandidates();
    DIAG.candidates = CANDIDATE_ORDER.length;
    if (added > 0) {
      progress('扫描到新增候选 ' + added + '（共 ' + CANDIDATE_ORDER.length + '）');
      probeUnprobed();
      return;
    }
    // 没有新候选也要把未探测过的候选试掉（首次进入时 scan 已经加过）
    probeUnprobed();
  }

  function probeUnprobed() {
    if (TEMPLATE) { return; }
    var years = termGuess();
    if (!years || !CANDIDATE_ORDER.length) {
      DIAG.probeFailed = !years;
      return;
    }
    DIAG.years = years;
    var queue = sortedCandidates().filter(function (c) { return !PROBED[c.value]; });
    if (!queue.length) { return; }
    (function next(index) {
      if (TEMPLATE || index >= queue.length) { return; }
      var cand = queue[index];
      PROBED[cand.value] = true;
      probeCandidate(cand.value, years).then(function (ok) {
        progress('探测 ' + cand.value.slice(0, 8) + (ok ? ' 命中' : ' 未命中'));
        if (ok) { adoptTemplate(cand.value, cand.source || 'scan', years); }
        else { next(index + 1); }
      });
    })(0);
  }
  window.__yunongAutoStart = function () { STAGE_ENTERED_AT = Date.now(); progress('手动触发抓取'); rescan(); };

  // 阶段 watcher：登录页期间只报 login；离开登录页才切 capture 并开始自动发现
  // ---- 最后手段：滚动/点击含"课表"的卡片，切换周选择器 ----
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

  setTimeout(function () { if (!TEMPLATE && STAGE === 'capture') { autoTrigger(); } }, 3000);
  setTimeout(function () { if (!TEMPLATE && STAGE === 'capture') { autoTrigger(); } }, 8000);
  window.__yunongFetchSchedule = function () {
    if (isLoginPage()) {
      post({ type: 'portal_hint', code: 'LOGIN_PENDING', message: '请先在页面里完成登录', diagnostics: diagnosticsJson() });
      return;
    }
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
              progress('第 ' + week + ' 周数据返回');
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
