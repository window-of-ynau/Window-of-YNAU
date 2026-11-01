// 校务系统内嵌浏览器 + 真实登录（W6 第一步：会话持久化 + 登录态检测）。
//
// 会话复用：Windows 走 WebView2 持久化 user data 目录（见 `webview_session.dart`），
// Android 走系统 CookieManager；Cookie 只在本机，不进 SQLite / JSON / 日志。
//
// 登录态判定：注入探针，只观察 DOM/可见文本（课表表头、学号姓名栏、密码框），
// 从不读 `document.cookie`；拿不到信号时落到 `unknown` 并明确提示，不猜、不死等。

import 'dart:async';
import 'dart:collection';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_inappwebview/flutter_inappwebview.dart';

import '../core/timetable_service.dart';
import 'webview_session.dart';

/// 一次抓取会话的状态消息。
enum FetchStage { loginDetected, grabbed, htmlChanged }

/// WebView 版取数实现。
class WebViewSchoolFetcher implements SchoolFetcher {
  WebViewSchoolFetcher({this.timeout = const Duration(minutes: 5)});

  /// 登录超时：用户开了页面不登录也不抓，别把 UI 永远卡住。
  final Duration timeout;

  /// App 根导航 key（WebView 页面要全屏 push 出来）。
  static final GlobalKey<NavigatorState> navigatorKey = GlobalKey<NavigatorState>();

  @override
  bool get isAvailable => WebViewSession.isSupportedPlatform;

  @override
  String? get unavailableReason => isAvailable ? null : '当前平台没有内置浏览器';

  @override
  Future<FetchedPage> fetchTimetablePage({required String entryUrl}) {
    final navigator = navigatorKey.currentState;
    if (!isAvailable || navigator == null) {
      throw FetchException(
        code: 'NO_WEBVIEW',
        userHint: '当前平台不支持内置浏览器',
        message: unavailableReason ?? '导航未就绪',
      );
    }
    final completer = Completer<FetchedPage>();
    var closed = false;
    void close() {
      if (closed) {
        return;
      }
      closed = true;
      navigator.pop();
    }

    unawaited(navigator.push<void>(MaterialPageRoute<void>(
      builder: (BuildContext context) => CampusWebViewPage(
        entryUrl: entryUrl,
        onFetched: (FetchedPage page) {
          if (!completer.isCompleted) {
            completer.complete(page);
          }
          close();
        },
        onError: (FetchException error) {
          if (!completer.isCompleted) {
            completer.completeError(error);
          }
          close();
        },
      ),
    )));

    return completer.future.timeout(timeout, onTimeout: () {
      close();
      throw const FetchException(
        code: 'LOGIN_TIMEOUT',
        userHint: '登录超时，请重试',
        message: '5 分钟内没有完成登录/抓取',
      );
    });
  }
}

/// 抓取用的全屏页面（由 `WebViewSchoolFetcher` push 出来）。
class CampusWebViewPage extends StatefulWidget {
  const CampusWebViewPage({
    super.key,
    required this.entryUrl,
    required this.onFetched,
    required this.onError,
    this.onStage,
  });

  final String entryUrl;
  final void Function(FetchedPage page) onFetched;
  final void Function(FetchException error) onError;
  final void Function(FetchStage stage)? onStage;

  @override
  State<CampusWebViewPage> createState() => _CampusWebViewPageState();
}

class _CampusWebViewPageState extends State<CampusWebViewPage> {
  InAppWebViewController? _controller;
  WebViewEnvironment? _environment;
  bool _busy = true;
  LoginProbe _probe = const LoginProbe(
    state: CampusLoginState.unknown,
    reason: 'probing',
  );

  @override
  void initState() {
    super.initState();
    // Windows 必须在创建 WebView 之前指定 user data folder（Cookie 就存在那里）
    WebViewSession.ensureEnvironment().then((WebViewEnvironment? environment) {
      if (mounted) {
        setState(() => _environment = environment);
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        title: const Text('登录校务系统'),
        actions: <Widget>[
          TextButton(
            onPressed: () => widget.onError(
              const FetchException(code: 'CANCELLED', userHint: '已取消抓取'),
            ),
            child: const Text('取消'),
          ),
        ],
      ),
      body: Column(
        children: <Widget>[
          if (_busy) const LinearProgressIndicator(),
          Material(
            color: theme.colorScheme.surfaceContainerHighest,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Row(
                    children: <Widget>[
                      Icon(
                        _probe.state == CampusLoginState.loggedIn
                            ? Icons.check_circle_outline
                            : Icons.info_outline,
                        size: 18,
                      ),
                      const SizedBox(width: 8),
                      Expanded(
                        child: Text(_probe.hint, style: theme.textTheme.bodySmall),
                      ),
                      OutlinedButton(
                        onPressed: () async {
                          await _controller?.evaluateJavascript(
                            source: 'window.__yunongGrab && window.__yunongGrab();',
                          );
                        },
                        child: const Text('抓取当前页'),
                      ),
                    ],
                  ),
                  const SizedBox(height: 4),
                  Text(
                    '会话只存在本机（${WebViewSession.storageDescription}），'
                    '不上传、不打印、不写进课表数据库。',
                    style: theme.textTheme.bodySmall?.copyWith(color: theme.hintColor),
                  ),
                ],
              ),
            ),
          ),
          Expanded(
            child: _environment == null && WebViewSession.needsEnvironment
                ? const Center(child: Text('正在准备本地会话目录…'))
                : InAppWebView(
                    webViewEnvironment: _environment,
                    initialUrlRequest: URLRequest(url: WebUri(widget.entryUrl)),
                    initialSettings: WebViewSession.settings(),
                    initialUserScripts: UnmodifiableListView<UserScript>(<UserScript>[
                      UserScript(
                        source: kCampusProbeScript,
                        injectionTime: UserScriptInjectionTime.AT_DOCUMENT_END,
                      ),
                    ]),
                    onWebViewCreated: (InAppWebViewController controller) {
                      _controller = controller;
                      controller.addJavaScriptHandler(
                        handlerName: 'yunongBridge',
                        callback: _onBridgeMessage,
                      );
                    },
                    onLoadStop: (_, WebUri? url) async {
                      if (mounted) {
                        setState(() => _busy = false);
                      }
                      await _controller?.evaluateJavascript(
                        source: 'window.__yunongProbeNow && window.__yunongProbeNow();',
                      );
                    },
                  ),
          ),
        ],
      ),
    );
  }

  /// JS bridge：**只接受白名单字段**，Cookie 一律不接受。
  dynamic _onBridgeMessage(List<dynamic> args) {
    if (args.isEmpty) {
      return null;
    }
    final raw = args.first;
    if (raw is! String || raw.isEmpty) {
      return null;
    }
    Object? decoded;
    try {
      decoded = jsonDecode(raw);
    } on FormatException {
      return null;
    }
    if (decoded is! Map<String, dynamic>) {
      return null;
    }
    switch (decoded['type']) {
      case 'login_state':
        final probe = parseLoginProbe(decoded);
        if (mounted) {
          setState(() => _probe = probe);
        }
        widget.onStage?.call(FetchStage.loginDetected);
      case 'login_done':
        widget.onStage?.call(FetchStage.loginDetected);
      case 'timetable':
        final html = decoded['html'];
        if (html is! String || html.trim().isEmpty) {
          widget.onError(const FetchException(
            code: 'HTML_CHANGED',
            userHint: '学校页面可能已更新，请反馈',
            message: '抓到空表格',
          ));
          return null;
        }
        widget.onStage?.call(FetchStage.grabbed);
        widget.onFetched(FetchedPage(
          html: html,
          url: (decoded['path'] as String?) ?? '',
        ));
      case 'error':
        final code = (decoded['code'] as String?) ?? 'HTML_CHANGED';
        widget.onError(FetchException(
          code: code,
          userHint: code == 'HTML_CHANGED'
              ? '学校页面可能已更新，请反馈'
              : '抓取失败，请重试',
          message: decoded['message'] as String?,
        ));
      default:
        break;
    }
    return null;
  }
}
