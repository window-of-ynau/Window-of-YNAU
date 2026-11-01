// WebView JS bridge 的**纯 Dart** 部分：解析消息 → 驱动取数状态机。
//
// 抽出来的原因：WebView 本身在测试里起不来（平台通道），但"消息怎么解释"是逻辑，
// 必须能在 CI 里用纯 Dart 测（见 `test/campus_bridge_test.dart`）。
//
// 白名单（隐私）：只接受 `type` / `state` / `reason` / `path` / `html` / `code` / `message`，
// **不接受也不解析任何 Cookie、表单值、完整 URL**。

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
  });

  final void Function(FetchedPage page) onFetched;
  final void Function(FetchException error) onError;
  final void Function(LoginProbe probe)? onLoginState;
  final void Function(FetchStage stage)? onStage;

  /// 捕获到课表接口模板（cardId/years/weekcount 都是运行时拿的）
  final void Function(ScheduleTemplate template)? onScheduleTemplate;

  /// 批量拉周完成（原样交给 Rust 合并）
  final void Function(ScheduleApiResult result)? onScheduleApi;

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
          message: _formatDiagnostics(message['diagnostics']),
        ));
        return true;

      case 'portal_hint':
        _fail(FetchException(
          code: 'PORTAL_PAGE',
          userHint: '当前是门户页，需要先打开「课表」卡片',
          message: _formatDiagnostics(message['diagnostics']),
        ));
        return true;
      case 'schedule_template':
        final template = ScheduleTemplate.fromMessage(message);
        if (template.isValid) {
          onScheduleTemplate?.call(template);
        }
        return true;

      case 'schedule_api':
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
        if (html is! String || html.trim().isEmpty) {
          _fail(const FetchException(
            code: 'HTML_CHANGED',
            userHint: '学校页面可能已更新，请反馈',
            message: '抓到空表格',
          ));
          return true;
        }
        if (_delivered) {
          return true;
        }
        _delivered = true;
        onStage?.call(FetchStage.grabbed);
        onFetched(FetchedPage(
          html: html,
          url: (message['path'] as String?) ?? '',
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
String _formatDiagnostics(Object? raw) {
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
  final probeResults = raw['probeResults'];
  if (probeResults is List && probeResults.isNotEmpty) {
    final tried = probeResults.take(12).map((dynamic item) => '$item').join(',');
    parts.add('探测 $tried');
  } else if (raw['probeFailed'] == true) {
    parts.add('探测全失败');
  }
  final years = raw['years'];
  if (years is String && years.isNotEmpty) {
    parts.add('学期 $years');
  } else {
    parts.add('学期未取到');
  }
  return parts.join(' · ');
}