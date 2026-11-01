// WebView 会话（Cookie）持久化 + 登录态探针。
//
// # Cookie 存在哪（隐私边界，ADR-11）
//
// | 平台 | 存放位置 | 谁管 |
// |---|---|---|
// | Windows | `%LOCALAPPDATA%\yunong\webview\`（WebView2 user data folder） | 操作系统账户隔离 |
// | Android | 应用私有数据目录（系统管理，`.../app_webview/`） | 应用沙箱 |
//
// **不在** SQLite、**不在**任何 JSON、**不进**日志、**不上传**。
// 端侧只读"页面里可见的业务元素"来判断登录态，从不读 `document.cookie`。
//
// # 为什么登录态要靠"业务元素"而不是 URL
//
// 校务系统的 CAS 登录会在 `jwxt` / `qzjwxt` / 统一身份认证之间来回跳，URL 变化
// 既不代表登录成功（可能有中间跳转页），也不代表失败（可能刚跳回业务页）。
// 而"课表表头/学号姓名栏"只在登录后存在，"密码框"只在登录前存在——这两类是**业务事实**，
// 抗改版能力比 URL 强得多（改版最多让选择器失配，会落到 `unknown` 并明确提示，而不是误判已登录）。

import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_inappwebview/flutter_inappwebview.dart';

/// 登录态三态。
///
/// `unknown` 是**故意留的**：探针拿不到任何信号时（页面改版、加载中、空白页）
/// 不能猜，也不能死等——UI 要把它显示成"没检测到，请手动抓取/反馈"。
enum CampusLoginState {
  /// 没有任何可信信号（加载中 / 页面改版 / 非校务页面）
  unknown,

  /// 看到登录表单或明确的未登录提示 → 需要用户去登录
  needLogin,

  /// 看到业务元素（课表表格 / 学号姓名栏）→ 已登录
  loggedIn,
}

/// 一次探针结果。
@immutable
class LoginProbe {
  const LoginProbe({required this.state, required this.reason, this.path = ''});

  final CampusLoginState state;

  /// 命中的信号名（`timetable` / `student_bar` / `login_form` / `login_hint` / `no_signal`），
  /// 用于提示与排障；**不含**任何页面内容。
  final String reason;

  /// 只保留 path（去掉 query/fragment：CAS 的 `ticket` 属于凭据，绝不能带出来）。
  final String path;

  /// 是否已登录。
  bool get isLoggedIn => state == CampusLoginState.loggedIn;

  /// 给用户看的一句话。
  String get hint => switch (state) {
        CampusLoginState.loggedIn =>
          reason == 'timetable' ? '已登录，正在读取课表…' : '已登录，正在打开课表…',
        CampusLoginState.needLogin => '请在校务系统里完成登录（登录成功后会自动读取课表）',
        CampusLoginState.unknown => '没检测到登录状态，页面可能改版；可点"抓取当前页"或反馈',
      };

  @override
  String toString() => 'LoginProbe($state, $reason, $path)';
}

/// 探针脚本：只观察 DOM / 可见文本，**绝不读取 Cookie**。
///
/// 回传：`{type:'login_state', state:'logged_in'|'need_login'|'unknown', reason, path}`
/// 以及（拿到表格时）`{type:'timetable', html}` / `{type:'error', code}`。
const String kCampusProbeScript = r'''
(function () {
  if (window.__yunongProbeReady) { return; }
  window.__yunongProbeReady = true;

  function bridge(payload) {
    try {
      if (!window.flutter_inappwebview) { return false; }
      // 白名单：只回传登录态与课表 HTML；不读、不回传 Cookie / 表单值 / 完整 URL
      window.flutter_inappwebview.callHandler('yunongBridge', JSON.stringify(payload));
      return true;
    } catch (e) { return false; }
  }

  function has(selector) {
    try { return !!document.querySelector(selector); } catch (e) { return false; }
  }
  function bodyText() {
    return document.body ? (document.body.innerText || '') : '';
  }
  function safePath() {
    // 去掉 query/hash：CAS 的 ticket 在 query 里，属于凭据
    try { return location.pathname || ''; } catch (e) { return ''; }
  }
  function findTable() {
    return document.querySelector('table#timetable')
        || document.querySelector('table.timetable')
        || document.querySelector('#timetable');
  }

  function detect() {
    // ① 业务元素优先：课表表头（登录后才有）
    if (findTable()) { return { state: 'logged_in', reason: 'timetable' }; }
    // ② 学号/姓名栏（登录后才有）；顺带兜住"课表在用 iframe 渲染"的情况
    if (has('#xh_id') || has('.user-info') || has('.stu-name') || has('#topMenu')) {
      return { state: 'logged_in', reason: 'student_bar' };
    }
    if (has('iframe')) {
      try {
        var inner = document.querySelector('iframe').contentDocument;
        if (inner && (inner.querySelector('table#timetable') || inner.querySelector('.kbcontent'))) {
          return { state: 'logged_in', reason: 'timetable_iframe' };
        }
      } catch (e) { /* 跨域 iframe 读不到，忽略 */ }
    }
    var text = bodyText();
    if (/学号[：:]\s*\d{6,}/.test(text) || /姓名[：:]\s*\S{2,4}/.test(text)) {
      return { state: 'logged_in', reason: 'student_bar' };
    }
    // ③ 登录表单
    if (has('input[type=password]') || has('#casLoginForm') || has('form[action*="login"]')
        || has('#username') && has('#password')) {
      return { state: 'need_login', reason: 'login_form' };
    }
    if (/请登录|未登录|登录超时|重新登录/.test(text)) {
      return { state: 'need_login', reason: 'login_hint' };
    }
    return { state: 'unknown', reason: 'no_signal' };
  }

  var lastKey = '';
  function report(force) {
    var d = detect();
    var path = safePath();
    var key = d.state + '|' + d.reason + '|' + path;
    if (!force && key === lastKey) { return; }
    lastKey = key;
    bridge({ type: 'login_state', state: d.state, reason: d.reason, path: path });
  }

  // 抓取：拿到表格就回传 outerHTML；拿不到明确报 HTML_CHANGED（不冒充"这学期没课"）
  window.__yunongGrab = function () {
    var table = findTable();
    if (!table) {
      bridge({ type: 'error', code: 'HTML_CHANGED',
               message: '当前页面找不到课表表格（table#timetable）' });
      return;
    }
    bridge({ type: 'timetable', html: table.outerHTML, path: safePath() });
  };

  report(true);
  setInterval(function () { report(false); }, 1200);
  window.__yunongProbeNow = function () { report(true); };
})();
''';

/// 把 bridge 消息解析成 [`LoginProbe`]（字段白名单 + 未知值兜底成 `unknown`）。
LoginProbe parseLoginProbe(Map<String, dynamic> message) {
  final state = switch (message['state']) {
    'logged_in' => CampusLoginState.loggedIn,
    'need_login' => CampusLoginState.needLogin,
    _ => CampusLoginState.unknown,
  };
  final reason = (message['reason'] as String?)?.trim() ?? '';
  final path = (message['path'] as String?) ?? '';
  return LoginProbe(
    state: state,
    reason: reason.isEmpty ? 'no_signal' : reason,
    path: _pathOnly(path),
  );
}

/// 双保险：即使脚本漏了，也把 path 里的 query/fragment 去掉（CAS 的 ticket 是凭据）。
String _pathOnly(String value) {
  if (value.isEmpty) {
    return '';
  }
  var path = value;
  final hash = path.indexOf('#');
  if (hash >= 0) {
    path = path.substring(0, hash);
  }
  final question = path.indexOf('?');
  if (question >= 0) {
    path = path.substring(0, question);
  }
  return path;
}

/// WebView 会话/环境（Cookie 持久化的唯一入口）。
class WebViewSession {
  WebViewSession._();

  /// 有没有可用的内嵌浏览器实现。
  static bool get isSupportedPlatform =>
      Platform.isWindows || Platform.isAndroid || Platform.isIOS;

  /// 是否需要在创建 WebView 之前显式建环境（只有 Windows/WebView2 需要）。
  static bool get needsEnvironment => Platform.isWindows;
  static const String _appDirName = 'yunong';
  static const String _profileDirName = 'webview';

  static String? _userDataDir;
  static Future<WebViewEnvironment?>? _environment;

  /// Windows 上的持久化 user data 目录。
  ///
  /// **必须**放在应用数据目录（而不是临时目录）：WebView2 把 Cookie / localStorage
  /// 都存在这里，放临时目录等于每次重启都要求用户重新登录。
  static String userDataDir() {
    final cached = _userDataDir;
    if (cached != null) {
      return cached;
    }
    final env = Platform.environment;
    final base = env['LOCALAPPDATA'] ??
        env['APPDATA'] ??
        // 极端情况下（无环境变量的服务账户）退到可执行文件目录，**不用临时目录**。
        File(Platform.resolvedExecutable).parent.path;
    final dir = <String>[base, _appDirName, _profileDirName].join(Platform.pathSeparator);
    Directory(dir).createSync(recursive: true);
    _userDataDir = dir;
    return dir;
  }

  /// 当前平台的会话存放说明（给 UI/日志用的**文字**，不含任何 Cookie 值）。
  static String get storageDescription => switch (defaultTargetPlatform) {
        TargetPlatform.windows => 'Windows：WebView2 用户数据目录 $userDataDir()',
        TargetPlatform.android => 'Android：应用私有目录（系统管理）',
        _ => '当前平台无持久化会话',
      };

  /// 创建（并复用）Windows 的 `WebViewEnvironment`；其它平台返回 `null`（用系统默认）。
  ///
  /// 只在 Windows 上需要：WebView2 的 user data folder 必须在创建 WebView 之前指定。
  static Future<WebViewEnvironment?> ensureEnvironment() {
    if (!Platform.isWindows) {
      return Future<WebViewEnvironment?>.value();
    }
    return _environment ??= _createWindowsEnvironment();
  }

  static Future<WebViewEnvironment?> _createWindowsEnvironment() async {
    final dir = userDataDir();
    final environment = await WebViewEnvironment.create(
      settings: WebViewEnvironmentSettings(userDataFolder: dir),
    );
    return environment;
  }

  /// 供 UI 展示/排障：WebView2 运行时版本（拿不到就返回 null）。
  static Future<String?> runtimeVersion() async {
    if (!Platform.isWindows) {
      return null;
    }
    try {
      final version = await WebViewEnvironment.getAvailableVersion();
      return version;
    } on Object {
      return null;
    }
  }

  /// 会话相关的 WebView 设置。
  ///
  /// - `thirdPartyCookiesEnabled`：CAS 与业务系统可能不同域，必须允许第三方 Cookie，
  ///   否则登录成功但业务页仍是未登录；
  /// - `domStorageEnabled` / `databaseEnabled`：部分教务页面用 localStorage 存 token；
  /// - 不打开 `javaScriptCanOpenWindowsAutomatically`（减少弹窗/钓鱼面）。
  static InAppWebViewSettings settings() => InAppWebViewSettings(
        javaScriptEnabled: true,
        thirdPartyCookiesEnabled: true,
        domStorageEnabled: true,
        databaseEnabled: true,
        // 页面里的 JS 只能通过我们注册的 bridge 与端侧通信，不允许直接拿到 Flutter 侧对象
        isInspectable: false,
      );
}
