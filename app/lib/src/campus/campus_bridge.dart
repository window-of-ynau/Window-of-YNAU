// WebView JS bridge 的**纯 Dart** 部分：解析消息 → 驱动取数状态机。
//
// 抽出来的原因：WebView 本身在测试里起不来（平台通道），但"消息怎么解释"是逻辑，
// 必须能在 CI 里用纯 Dart 测（见 `test/campus_bridge_test.dart`）。
//
// 白名单（隐私）：只接受 `type` / `state` / `reason` / `path` / `html` / `code` / `message`，
// **不接受也不解析任何 Cookie、表单值、完整 URL**。

import 'dart:convert';

import '../core/timetable_service.dart';
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
  });

  final void Function(FetchedPage page) onFetched;
  final void Function(FetchException error) onError;
  final void Function(LoginProbe probe)? onLoginState;
  final void Function(FetchStage stage)? onStage;

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
    if (error.code != 'HTML_CHANGED') {
      _delivered = true;
    }
    onError(error);
  }
}

/// 抓取失败码 → 用户看得懂的一句话（不要弹异常栈）。
String describeFetchFailure(String code) => switch (code) {
      'HTML_CHANGED' => '学校页面可能已更新，请反馈',
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
