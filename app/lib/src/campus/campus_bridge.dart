// WebView JS bridge 的**纯 Dart** 部分：解析消息 → 驱动取数状态机。
//
// 抽出来的原因：WebView 本身在测试里起不来（平台通道），但"消息怎么解释"是逻辑，
// 必须能在 CI 里用纯 Dart 测（见 `test/campus_bridge_test.dart`）。
//
// 白名单（隐私）：只接受 `type` / `state` / `reason` / `path` / `html` / `code` / `message`，
// **不接受也不解析任何 Cookie、表单值、完整 URL**。

import 'dart:async';
import 'dart:convert';

import '../core/timetable_service.dart';
import 'portal_api.dart';
import 'webview_session.dart';

/// 抓取阶段（UI 用来显示进度）。
enum FetchStage { probing, loginDetected, grabbing, grabbed }

/// bridge 消息处理器。
class CampusBridge {
  CampusBridge({
    required this.onFetched,
    required this.onError,
    this.onLoginState,
    this.onStage,
    this.onScheduleTemplate,
    this.onScheduleApi,
    this.onStageChange,
    this.grabHtml,
    this.retryDelay = const Duration(milliseconds: 800),
    this.minHtmlLength = 2000,
    this.requireTimetableKeyword = true,
  });

  final void Function(FetchedPage page) onFetched;

  /// 【t148】页面侧可重复调用的抓取入口（`window.__yunongGrabHtml()`）。
  ///
  /// 首次收到的 html 若是"空壳"，用它**重取一次**（不重新导航），再判失败。
  final Future<String?> Function()? grabHtml;

  /// 重取前的等待（默认 800ms）。
  final Duration retryDelay;

  /// "空壳"判定阈值（默认 2000 字符；测试可调小）。
  final int minHtmlLength;

  /// 是否要求 HTML 含课表关键字（测试可关掉）。
  final bool requireTimetableKeyword;

  /// 按当前配置判断一条 html 是否算"空壳"。
  bool _isHollow(String html) {
    final HtmlFetchAttempt attempt = HtmlFetchAttempt(html: html, tries: 1);
    if (html.trim().isEmpty) {
      return true;
    }
    if (html.length < minHtmlLength) {
      return true;
    }
    return requireTimetableKeyword && !attempt.hasTimetableMark;
  }

  /// 是否已经用过那一次重取机会（每次抓取只重试一次）。
  bool _htmlRetryUsed = false;

  /// 最近一次交付/失败的 html 诊断行（`html 长度=N / 是否含课表关键字=…（第 N 次）`）。
  String? lastHtmlDiagnostics;
  final void Function(FetchException error) onError;
  final void Function(LoginProbe probe)? onLoginState;
  final void Function(FetchStage stage)? onStage;

  /// 捕获到课表接口模板（cardId/years/weekcount 都是运行时拿的）
  final void Function(ScheduleTemplate template)? onScheduleTemplate;

  /// 批量拉周完成（原样交给 Rust 合并）
  final void Function(ScheduleApiResult result)? onScheduleApi;

  /// Dart 侧收到的 schedule_api 消息数（用于和脚本侧探测计数对照）。
  int scheduleApiCount = 0;  /// t98：Dart 主动拉取 `window.__yunongState` 的统计（诊断用）。  int pullCount = 0;  int lastStateAgeSeconds = -1;  int stateEventsConsumed = 0;

  /// t98：Dart 主动拉取 `window.__yunongState` 的统计（诊断用）。
  int pullCount = 0;
  int lastStateAgeSeconds = -1;
  int stateEventsConsumed = 0;

  /// 阶段变化：`login`（等用户登录，**不抓取**）/ `capture`（登录完成，开始抓）
  final void Function(String stage, Map<String, dynamic>? diagnostics)? onStageChange;

  /// 已经拿到过课表就不再重复触发（抓取脚本可能多轮上报）。
  bool _delivered = false;

  /// 处理一条 bridge 文本；返回是否被消费（未知消息返回 false 以便前向兼容）。
  bool handleRaw(Object? raw) {
    if (raw is! String || raw.isEmpty) {
      return false;
    }
    Object? decoded;
    try {
      decoded = jsonDecode(raw);
    } on FormatException {
      return false;
    }
    if (decoded is! Map<String, dynamic>) {
      return false;
    }
    return handle(decoded);
  }

  /// 处理已解析的消息。
  bool handle(Map<String, dynamic> message) {
    switch (message['type']) {
      case 'login_state':
        onLoginState?.call(parseLoginProbe(message));
        return true;

      case 'login_done':
        // 兼容旧脚本：等价于"疑似已登录"
        onLoginState?.call(const LoginProbe(
          state: CampusLoginState.loggedIn,
          reason: 'legacy_signal',
        ));
        return true;

      case 'capture_timeout':
        _fail(FetchException(
          code: 'CAPTURE_TIMEOUT',
          userHint: '请在页面里打开「课表」卡片，或切换一次周次后重试',
          message: _formatDiagnostics(          message['diagnostics'],          dartScheduleApiCount: scheduleApiCount,          pullCount: pullCount,          stateAgeSeconds: lastStateAgeSeconds,          stateEventsConsumed: stateEventsConsumed,        ),
        ));
        return true;

      case 'portal_hint':
        _fail(FetchException(
          code: 'PORTAL_PAGE',
          userHint: '当前是门户页，需要先打开「课表」卡片',
          message: _formatDiagnostics(          message['diagnostics'],          dartScheduleApiCount: scheduleApiCount,          pullCount: pullCount,          stateAgeSeconds: lastStateAgeSeconds,          stateEventsConsumed: stateEventsConsumed,        ),
        ));
        return true;
      case 'stage':
        final stage = (message['stage'] as String?) ?? '';
        if (stage.isNotEmpty) {
          onStageChange?.call(
            stage,
            message['diagnostics'] is Map<String, dynamic>
                ? message['diagnostics'] as Map<String, dynamic>
                : null,
          );
        }
        return true;

      case 'schedule_template':
        final template = ScheduleTemplate.fromMessage(message);
        if (template.isValid) {
          onScheduleTemplate?.call(template);
        }
        return true;

      case 'schedule_api':
        scheduleApiCount += 1;
      final result = ScheduleApiResult.fromMessage(message);
        if (_delivered) {
          return true;
        }
        _delivered = true;
        onStage?.call(FetchStage.grabbed);
        onScheduleApi?.call(result);
        return true;

      case 'auth_required':
        _fail(FetchException(
          code: 'AUTH_REQUIRED',
          userHint: describeFetchFailure('AUTH_REQUIRED'),
          message: message['message'] as String?,
        ));
        return true;
      case 'timetable':
        final html = message['html'];
        final String path = (message['path'] as String?) ?? '';
        final String htmlText = html is String ? html : '';
        final HtmlFetchAttempt attempt = HtmlFetchAttempt(html: htmlText, tries: 1);
        if (!_isHollow(htmlText)) {
          _deliver(attempt, path);
          return true;
        }
        // t148：空壳不再立刻失败 —— 先用页面侧的可重复入口**重取一次**（800ms 后），
        // 仍不合格才报 HTML_CHANGED（诊断里带上次数与长度，便于定位）。
        if (!_htmlRetryUsed && grabHtml != null) {
          _htmlRetryUsed = true;
          unawaited(_retryHtml(path, first: attempt));
          return true;
        }
        _fail(FetchException(
          code: 'HTML_CHANGED',
          userHint: '学校页面可能已更新，请反馈',
          message: '抓到空表格（${attempt.diagnostics}）',
        ));
        return true;

      case 'error':
        final code = (message['code'] as String?) ?? 'HTML_CHANGED';
        _fail(FetchException(
          code: code,
          userHint: describeFetchFailure(code),
          message: message['message'] as String?,
        ));
        return true;

      default:
        // 未知消息直接忽略（脚本可以演化，端侧不能因此崩）
        return false;
    }
  }

  /// 交付一次合格的 HTML（只交付一次）。
  void _deliver(HtmlFetchAttempt attempt, String path) {
    if (_delivered) {
      return;
    }
    _delivered = true;
    lastHtmlDiagnostics = attempt.diagnostics;
    onStage?.call(FetchStage.grabbed);
    onFetched(FetchedPage(html: attempt.html, url: path));
  }

  /// t148：空壳 → 等 [retryDelay] 后用页面侧入口重取一次。
  Future<void> _retryHtml(String path, {required HtmlFetchAttempt first}) async {
    await Future<void>.delayed(retryDelay);
    String? html;
    try {
      html = await grabHtml?.call();
    } on Object {
      html = null;
    }
    final HtmlFetchAttempt second = HtmlFetchAttempt(html: html ?? '', tries: 2);
    if (!_isHollow(html ?? '')) {
      _deliver(second, path);
      return;
    }
    _fail(FetchException(
      code: 'HTML_CHANGED',
      userHint: '学校页面可能已更新，请反馈',
      message: '重试后仍是空表格：${first.diagnostics} → ${second.diagnostics}',
    ));
  }

  void _fail(FetchException error) {
    if (_delivered) {
      return;
    }
    // `HTML_CHANGED` 是"这次没找到表"：自动重试窗口内页面可能还在渲染，
    // 所以不锁定 delivered——只要后面某一轮真抓到了表，仍然算成功。
    if (error.code != 'HTML_CHANGED' &&
        error.code != 'CARD_NOT_FOUND' &&
        error.code != 'CAPTURE_TIMEOUT' &&
        error.code != 'PORTAL_PAGE') {
      _delivered = true;
    }
    onError(error);
  }
}

/// 抓取失败码 → 用户看得懂的一句话（不要弹异常栈）。
String describeFetchFailure(String code) => switch (code) {
      'HTML_CHANGED' => '学校页面可能已更新，请反馈',
      'CARD_NOT_FOUND' => '没抓到课表接口：请先在门户里打开一次课表卡片',
      'CAPTURE_TIMEOUT' => '请在页面里打开「课表」卡片，或切换一次周次后重试',
      'PORTAL_PAGE' => '当前是门户页，需要先打开「课表」卡片',
      'AUTH_REQUIRED' => '登录态已失效，请重新登录',
      'LOGIN_TIMEOUT' => '登录超时，请重试',
      'NO_WEBVIEW' => '当前平台不支持内置浏览器',
      'NETWORK' => '校务系统打不开，请检查校园网',
      'CERT' => '校务系统证书异常，请检查网络环境',
      'CANCELLED' => '已取消抓取',
      'TIMEOUT' => '抓取超时，请重试',
      _ => '抓取失败，请重试',
    };

/// 网络/证书错误 → 我们自己的错误码（把平台错误文案收敛成可读提示）。
String classifyWebViewError(String? description, {String? type}) {
  final text = (description ?? '').toLowerCase();
  final kind = (type ?? '').toLowerCase();
  if (text.contains('cert') || kind.contains('cert')) {
    return 'CERT';
  }
  if (text.contains('name_not_resolved') ||
      text.contains('host') ||
      kind.contains('hostlookup') ||
      text.contains('dns')) {
    return 'NETWORK';
  }
  if (text.contains('timeout') || kind.contains('timeout')) {
    return 'NETWORK';
  }
  return 'NETWORK';
}

/// 把脚本带回的诊断信息压成一行（给人看，**不含任何凭据**）。
String _formatDiagnostics(  Object? raw, {  int dartScheduleApiCount = 0,  int pullCount = 0,  int stateAgeSeconds = -1,  int stateEventsConsumed = 0,}) {
  if (raw is! Map) {
    return '';
  }
  final parts = <String>[];
  final path = raw['path'];
  if (path is String && path.isNotEmpty) {
    parts.add('页面 $path');
  }
  final title = raw['title'];
  if (title is String && title.trim().isNotEmpty) {
    parts.add('标题 ${title.trim()}');
  }
  parts.add(raw['captured'] == true ? '已捕获接口' : '未捕获接口');
  final xhrSeen = raw['xhrSeen'];
  if (xhrSeen is num) {
    parts.add('XHR $xhrSeen 次');
  }
  final cardIdPrefix = raw['cardIdPrefix'];
  if (cardIdPrefix is String && cardIdPrefix.isNotEmpty) {
    parts.add('cardId ${cardIdPrefix}…');
  } else {
    parts.add('cardId 无');
  }
  final source = raw['cardIdSource'];
  if (source is String && source.isNotEmpty) {
    parts.add('来源 $source');
  }
  // 诊断三件套：hook 是否装上、试了几个候选、每个候选的状态码（只看前 8 位前缀）
  if (raw['hookInstalled'] == true) {
    parts.add('hook 已装');
  }
  final candidates = raw['candidates'];
  if (candidates is num) {
    parts.add('候选 $candidates 个');
  }
  // 探测明细：每个候选 `[前缀:状态码:判据]`，判据 rows7=命中 / empty=空数组 / missing=字段缺失 /
  // html=被踢回登录页 / http=非 2xx / auth / badjson / err
  final probeParams = raw['probeParams'];
  if (probeParams is Map) {
    parts.add('参数 years=${probeParams['years']} results=${probeParams['results']}');
  }
  // 两侧计数对照：脚本侧"探测了几次" vs Dart 侧"收到几条 schedule_api"。
  // 两者都 0 → 真的没发；脚本 >0 而 Dart=0 → 发了没上报（桥接问题）。
  final scriptProbes = raw['probeAttempts'];
  final scriptProbeCount = scriptProbes is num
      ? scriptProbes.toInt()
      : (raw['probeResults'] is List ? (raw['probeResults'] as List).length : 0);
  parts.add('桥接 脚本探测$scriptProbeCount 次 / Dart收到 $dartScheduleApiCount 条');
    parts.add('拉取 $pullCount 次 / 最后状态 age=${stateAgeSeconds}s / 已消费 $stateEventsConsumed 条');
  final probeResults = raw['probeResults'];
  if (probeResults is List && probeResults.isNotEmpty) {
    final tried = probeResults.take(12).map((dynamic item) => '[$item]').join(' ');
    parts.add('探测 $tried');
  } else if (raw['probeFailed'] == true) {
    parts.add('探测全失败');
  } else {
    parts.add('探测未发出');
  }
  final years = raw['years'];
  if (years is String && years.isNotEmpty) {
    parts.add('学期 $years');
  } else {
    parts.add('学期未取到');
  }
  return parts.join(' · ');
}
