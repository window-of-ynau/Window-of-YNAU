// 校务系统内嵌浏览器 + 真实登录 + 抓取（W6）。
//
// 流程：
//   打开校务课表页 → CAS 需要登录时就是登录页 → 用户在里面完成登录（含验证码/二次验证）
//   → 探针发现"业务元素"（课表表头 / 学号姓名栏）→ **自动**注入抓取 →
//   `table#timetable` 的 outerHTML 经 bridge 回传 → 关页面 → Rust 解析/落库/查询。
//
// 会话复用：Windows 走 WebView2 持久化 user data 目录、Android 走系统 CookieManager，
// 见 `webview_session.dart` 的表格（Cookie 只在本机，不进 SQLite / JSON / 日志）。
//
// 失败姿势：
// - 找不到表 → `HTML_CHANGED`（"学校页面可能已更新，请反馈"），不冒充"这学期没课"；
// - 5 分钟没登录成功/没抓到 → `LOGIN_TIMEOUT`（"登录超时，请重试"），可重试；
// - 网络/证书 → `NETWORK` / `CERT`，只给可读提示，不弹异常栈。

import 'dart:async';
import 'dart:collection';

import 'package:flutter/material.dart';
import 'package:flutter_inappwebview/flutter_inappwebview.dart';

import '../core/timetable_service.dart';
import 'campus_bridge.dart';
import 'webview_session.dart';

/// WebView 版取数实现（真实登录 + 会话复用）。
class WebViewSchoolFetcher implements SchoolFetcher {
  WebViewSchoolFetcher({this.timeout = const Duration(minutes: 5)});

  /// 登录超时：开了页面不登录也不抓，别把 UI 永远卡住（到期提示可重试）。
  final Duration timeout;

  /// App 根导航 key（WebView 页面要全屏 push 出来）。
  static final GlobalKey<NavigatorState> navigatorKey = GlobalKey<NavigatorState>();

  @override
  bool get isAvailable => WebViewSession.isSupportedPlatform;

  @override
  String? get unavailableReason =>
      isAvailable ? null : '当前平台没有内置浏览器';

  @override
  Future<FetchedPage> fetchTimetablePage({required String entryUrl}) {
    final navigator = navigatorKey.currentState;
    if (!isAvailable || navigator == null) {
      throw FetchException(
        code: 'NO_WEBVIEW',
        userHint: describeFetchFailure('NO_WEBVIEW'),
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

  /// 已登录但还没拿到课表时的重试抓取（最多 20 次 × 1.5s ≈ 30s）。
  Timer? _grabTimer;
  int _grabAttempts = 0;
  static const int _maxGrabAttempts = 20;

  bool _busy = true;
  LoginProbe _probe = const LoginProbe(
    state: CampusLoginState.unknown,
    reason: 'probing',
  );
  late final CampusBridge _bridge = CampusBridge(
    onFetched: widget.onFetched,
    onError: _onBridgeError,
    onLoginState: _onLoginState,
    onStage: (FetchStage stage) {
      widget.onStage?.call(stage);
      if (stage == FetchStage.grabbed) {
        _stopGrabLoop();
      }
    },
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
  void dispose() {
    _stopGrabLoop();
    super.dispose();
  }

  // ---------------------------------------------------------------------------
  // 登录态 → 自动取数
  // ---------------------------------------------------------------------------

  void _onLoginState(LoginProbe probe) {
    if (!mounted) {
      return;
    }
    setState(() {
      _probe = probe;
      _busy = probe.state == CampusLoginState.unknown && _busy;
    });
    if (probe.isLoggedIn) {
      // 已登录（会话复用成功，或刚登录完）→ 自动进取数，不需要用户再点按钮
      _startGrabLoop();
    } else if (probe.state == CampusLoginState.needLogin) {
      _stopGrabLoop();
    }
  }

  void _startGrabLoop() {
    if (_grabTimer != null) {
      return;
    }
    _grabAttempts = 0;
    _grabTimer = Timer.periodic(const Duration(milliseconds: 1500), (Timer timer) async {
      _grabAttempts += 1;
      if (_grabAttempts > _maxGrabAttempts) {
        _stopGrabLoop();
        // 已登录但 30s 都没抓到表格：多半是课表页改版/还没渲染完，明确报错而不是死等
        widget.onError(const FetchException(
          code: 'HTML_CHANGED',
          userHint: '学校页面可能已更新，请反馈',
          message: '已登录但 30 秒内没有找到课表表格，可点"抓取当前页"重试',
        ));
        return;
      }
      await _controller?.evaluateJavascript(
        source: 'window.__yunongGrab && window.__yunongGrab();',
      );
    });
    // 立刻抓一次，别等第一个 1.5s
    unawaited(_controller?.evaluateJavascript(
      source: 'window.__yunongGrab && window.__yunongGrab();',
    ));
  }

  void _stopGrabLoop() {
    _grabTimer?.cancel();
    _grabTimer = null;
  }

  void _onBridgeError(FetchException error) {
    if (error.code == 'HTML_CHANGED' && _grabTimer != null) {
      // 还在自动重试窗口内：单次没找到表不算失败（页面可能还在渲染/跳转）
      return;
    }
    _stopGrabLoop();
    widget.onError(error);
  }

  // ---------------------------------------------------------------------------
  // UI
  // ---------------------------------------------------------------------------

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
                      Icon(_probe.state == CampusLoginState.loggedIn
                          ? Icons.check_circle_outline
                          : Icons.info_outline, size: 18),
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
                        callback: (List<dynamic> args) {
                          _bridge.handleRaw(args.isEmpty ? null : args.first);
                          return null;
                        },
                      );
                    },
                    onLoadStop: (_, WebUri? url) async {
                      if (mounted) {
                        setState(() => _busy = false);
                      }
                      // 每次导航结束立刻探一次（登录成功后自动进入取数靠这条）
                      await _controller?.evaluateJavascript(
                        source: 'window.__yunongProbeNow && window.__yunongProbeNow();',
                      );
                    },
                    onReceivedError: (
                      _,
                      WebResourceRequest request,
                      WebResourceError error,
                    ) {
                      if (request.isForMainFrame != true) {
                        return;
                      }
                      final String code = classifyWebViewError(
                        error.description,
                        type: error.type.toString(),
                      );
                      _stopGrabLoop();
                      widget.onError(FetchException(
                        code: code,
                        userHint: describeFetchFailure(code),
                        message: error.description,
                      ));
                    },
                  ),
          ),
        ],
      ),
    );
  }
}
