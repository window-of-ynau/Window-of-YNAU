// 校务系统内嵌浏览器 + 抓取（ADR-11：Cookie 只留本机）。
//
// 流程：打开校务入口 → **用户自己**在里面登录 → 注入脚本检测到课表表格 →
// 把 `table#timetable` 的 outerHTML 通过 JS bridge 回传 → 关页面 → 同一套
// Rust 解析/落库/查询（`TimetableService`）。
//
// 隐私红线（advisor 要求写死在这里）：
// - **不读 `document.cookie`、不回传 Cookie、不写 Cookie 到日志**；
// - Cookie 由 WebView 自己按平台存储（Windows: WebView2 用户数据目录；Android: 应用沙箱）；
// - 回传载荷只有：`type` + 课表表格 HTML + 当前 URL（URL 里不含凭据）。
//
// 失败姿势：抓不到表 → 明确回 `error(HTML_CHANGED)`，绝不返回空 HTML 当"这学期没课"。

import 'dart:async';
import 'dart:collection';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_inappwebview/flutter_inappwebview.dart';

import '../core/timetable_service.dart';

/// 注入到每个页面的脚本。
///
/// 只做三件事：找课表表格、把它序列化、通过 bridge 回传；
/// 另外在检测到"已登录"特征时回报一次 `login_done`（UI 可以提示"已登录，正在找课表"）。
const String kYunongExtractionScript = r'''
(function () {
  if (window.__yunongReady) { return; }
  window.__yunongReady = true;

  function bridge(type, payload) {
    try {
      if (!window.flutter_inappwebview) { return false; }
      // 只回传结构化的课表 HTML / 状态，不回传任何 Cookie 或表单内容
      window.flutter_inappwebview.callHandler('yunongBridge',
        JSON.stringify(Object.assign({ type: type }, payload || {})));
      return true;
    } catch (e) { return false; }
  }

  function findTable() {
    return document.querySelector('table#timetable')
        || document.querySelector('table.timetable')
        || document.querySelector('#timetable');
  }

  var loggedInHint = ['我的课表', '学期', '课表查询', '退出登录'];
  function looksLoggedIn() {
    var text = document.body ? document.body.innerText || '' : '';
    for (var i = 0; i < loggedInHint.length; i++) {
      if (text.indexOf(loggedInHint[i]) >= 0) { return true; }
    }
    return !!findTable();
  }

  var loginReported = false;
  window.__yunongGrab = function () {
    var table = findTable();
    if (!table) {
      bridge('error', { code: 'HTML_CHANGED', message: '当前页面找不到课表表格（table#timetable）' });
      return;
    }
    bridge('timetable', { html: table.outerHTML, url: location.href });
  };

  var tries = 0;
  var timer = setInterval(function () {
    tries += 1;
    if (!loginReported && looksLoggedIn()) {
      loginReported = true;
      bridge('login_done', {});
    }
    if (findTable()) {
      clearInterval(timer);
      window.__yunongGrab();
      return;
    }
    // 60 秒还没有表格：交给用户点"抓取当前页"，不要自己判定失败
    if (tries > 75) { clearInterval(timer); }
  }, 800);
})();
''';

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
  bool get isAvailable => Platform.isWindows || Platform.isAndroid || Platform.isIOS;

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
        code: 'TIMEOUT',
        userHint: '登录超时，请重试',
        message: '等待课表页面超时',
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
  bool _busy = true;
  String _hint = '正在打开校务系统…请在里面完成登录';

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('登录校务系统'),
        actions: <Widget>[
          TextButton(
            onPressed: () => widget.onError(
              const FetchException(
                code: 'CANCELLED',
                userHint: '已取消抓取',
              ),
            ),
            child: const Text('取消'),
          ),
        ],
      ),
      body: Column(
        children: <Widget>[
          if (_busy) const LinearProgressIndicator(),
          Material(
            color: Theme.of(context).colorScheme.surfaceContainerHighest,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
              child: Row(
                children: <Widget>[
                  const Icon(Icons.info_outline, size: 18),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(_hint, style: Theme.of(context).textTheme.bodySmall),
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
            ),
          ),
          Expanded(
            child: InAppWebView(
              initialUrlRequest: URLRequest(url: WebUri(widget.entryUrl)),
              initialUserScripts: UnmodifiableListView<UserScript>(<UserScript>[
                UserScript(
                  source: kYunongExtractionScript,
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
              onLoadStop: (_, __) => setState(() {
                _busy = false;
                _hint = '已登录？我们会自动找课表；也可以点右侧"抓取当前页"';
              }),
              onReceivedError: (_, WebResourceRequest request, WebResourceError error) {
                if (request.isForMainFrame == true) {
                  widget.onError(FetchException(
                    code: 'LOAD_FAILED',
                    userHint: '校务系统打不开，请检查校园网',
                    message: error.description,
                  ));
                }
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
    if (raw is! String) {
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
      case 'login_done':
        widget.onStage?.call(FetchStage.loginDetected);
        if (mounted) {
          setState(() => _hint = '检测到已登录，正在抓取课表…');
        }
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
          url: (decoded['url'] as String?) ?? '',
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
        // 未知消息直接忽略（前向兼容）
        break;
    }
    return null;
  }
}
