// 【临时诊断】会话校验探针的入口弹窗 —— 验证后删除。
//
// # t140 重要收敛（owner 要求 + 实测结论）
//
// **校方域名（ssoo/one/jwxt/jwgl/cas.ynau.edu.cn）有动态 JS 防护**（瑞数一类）：
// 412 响应体里是 `document.createElement("section"); $_ts=window['$_ts'] …` 这类挑战脚本，
// 必须真实执行 JS 才能拿到响应。因此：
//
// - **禁止**从 Dart HTTP / 云端 / CLI 直连校方接口（`runSessionProbe` 对校方域名直接拒绝）；
// - 校方数据**只在 WebView 内**用 `fetch` 取（真实浏览器环境，天然过 JS 挑战）；
// - 每次点击**最多 2 个请求**（1 带会话 + 1 匿名对照），两次点击之间**冷却 10 秒**；
// - 已删除"三组请求头对照"与"四域扫描"两类批量探测（已被证明无效且增加风控暴露）。
//
// Cookie / ticket 仍不出设备：Cookie 只显示条数与名字/域，ticket 只显示 `ST-` 后 4 位。

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_inappwebview/flutter_inappwebview.dart';

import 'campus_entries.dart';
import 'card_id_hints.dart';
import 'portal_api.dart';
import 'session_cookie_diagnostics.dart';
import 'session_probe.dart';
import 'webview_session.dart';

/// 读取校务域当前会话 Cookie（可能为空：还没登录过）。
///
/// **必须传 `webViewEnvironment`**：插件 Windows 实现
/// （`flutter_inappwebview_windows-0.6.0/windows/cookie_manager.cpp:26-29`）在缺省环境下
/// 把 `webViewEnvironment` 解析成 `nullptr`，随后 `getCookies()`（同文件 `:211`/`:224`）
/// 调 `webViewEnvironment->getWebView()` 失败 → 静默返回空列表（"Cookie 恒为 0"的根因）。
Future<List<ProbeCookie>> loadCampusCookies() async {
  final List<ProbeCookie> cookies = <ProbeCookie>[];
  final WebViewEnvironment? environment = await WebViewSession.ensureEnvironment();
  for (final String url in <String>[
    CampusEntry.portal.url,
    'https://ssoo.ynau.edu.cn/',
  ]) {
    try {
      final List<Cookie> found = await CookieManager.instance(webViewEnvironment: environment)
          .getCookies(url: WebUri(url));
      for (final Cookie cookie in found) {
        if (cookies.any((ProbeCookie item) => item.name == '${cookie.name}')) {
          continue;
        }
        cookies.add(ProbeCookie('${cookie.name}', '${cookie.value}'));
      }
    } on Object {
      // 没有 WebView 会话/平台不支持：当作没有 Cookie，UI 会提示先登录。
    }
  }
  return cookies;
}

/// 按**精确 URL** 取 Cookie 元信息（只有名字/域，**没有值**）。
Future<List<CookieRef>> loadCampusCookieRefs(Uri url) async {
  final WebViewEnvironment? environment = await WebViewSession.ensureEnvironment();
  final List<Cookie> found = await CookieManager.instance(webViewEnvironment: environment)
      .getCookies(url: WebUri(url.toString()));
  return found
      .map((Cookie cookie) => CookieRef(name: '${cookie.name}', domain: '${cookie.domain ?? ''}'))
      .toList(growable: false);
}

class SessionProbeDialog extends StatefulWidget {
  const SessionProbeDialog({
    super.key,
    this.cookieLoader,
    this.cookieRefSource,
    this.jsProbe,
    this.inPageFetch,
    this.navigate,
    this.evaluateScript,
    this.cardIdProbeTimeout = const Duration(seconds: 5),
  });

  /// 测试注入；默认走真实 CookieManager。
  final Future<List<ProbeCookie>> Function()? cookieLoader;
  final CookieRefSource? cookieRefSource;
  final JsCookieLengthProbe? jsProbe;

  /// t140：**页内** fetch（真实浏览器环境）。默认用课表 WebView 注入的实现。
  final Future<InPageFetchSummary?> Function(String url, {required bool withCredentials})? inPageFetch;

  /// CAS 探针的导航注入点（默认用课表页 WebView）。
  final Future<void> Function(String url)? navigate;

  /// t143：`evaluateJavascript` 直接取值的注入点（默认用课表 WebView）。
  final ScriptEvaluator? evaluateScript;

  /// t143：探测超时（默认 5s；测试里可缩短）。
  final Duration cardIdProbeTimeout;

  @override
  State<SessionProbeDialog> createState() => _SessionProbeDialogState();
}

class _SessionProbeDialogState extends State<SessionProbeDialog> {
  /// 每轮最多 2 个请求（带会话 1 + 匿名对照 1）。
  static const int kMaxRequestsPerRun = 2;

  /// 两次请求之间的冷却（防止连点看起来像攻击）。
  static const Duration kCooldown = Duration(seconds: 10);

  /// CAS 探针超时与最多展示跳数。
  static const Duration kCasTimeout = Duration(seconds: 15);
  static const int kCasMaxHops = 5;

  /// t146：不再提供"目标 URL / 学期 / cardId"的手动输入（那是旧的 Dart HTTP 入口）。
  /// 现在请求固定走页内同款形状；cardId 由「自动探测」填入（内部状态，不展示输入框）。
  String _cardIdValue = '';
  String _yearsValue = '2026-2027-1';
  final TextEditingController _casBase = TextEditingController(text: CasTicketProbe.defaultCasBase);
  final TextEditingController _serviceUrl =
      TextEditingController(text: CasTicketProbe.defaultServiceUrl);

  List<ProbeCookie> _cookies = const <ProbeCookie>[];
  CookieDiagnostics? _diagnostics;
  InPageFetchSummary? _withCookie;
  InPageFetchSummary? _anonymous;
  CasTicketResult? _cas;

  int _requestsUsed = 0;

  /// 冷却剩余秒数（用计数器而不是 `DateTime.now()`：便于测试与避免时钟漂移）。
  int _cooldownLeftSeconds = 0;
  Timer? _cooldownTicker;
  bool _busy = false;
  bool _casRunning = false;
  Completer<void>? _casDone;
  Timer? _casTicker;
  String? _notice;

  /// t142：cardId 候选（页面内纯本地探测，最多 10 个）。
  List<CardIdHint> _cardIdHints = const <CardIdHint>[];
  String? _cardIdHintMessage;

  /// t143：探测进行中（只影响探测按钮，不复用 `_busy`，避免与其它按钮互相干扰）。
  bool _probing = false;

  @override
  void initState() {
    super.initState();
    _loadCookies();
  }

  @override
  void dispose() {

    _casBase.dispose();
    _serviceUrl.dispose();
    _cooldownTicker?.cancel();
    _casTicker?.cancel();
    super.dispose();
  }

  bool get _cooling => _cooldownLeftSeconds > 0;
  bool get _canRequest => !_busy && !_cooling && _requestsUsed < kMaxRequestsPerRun;

  Future<void> _loadCookies() async {
    setState(() => _busy = true);
    try {
      final List<ProbeCookie> cookies = await (widget.cookieLoader ?? loadCampusCookies)();
      if (!mounted) {
        return;
      }
      setState(() {
        _cookies = cookies;
        _notice = cookies.isEmpty ? '没有取到校务会话 Cookie：请先在「我的课表」里登录一次校务系统。' : null;
      });
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }

  /// 目标 URL：若填的是课表接口路径，则用 `years/cardId` 按页内同款形状拼好。
  Uri _target() {
    final Uri parsed = Uri.parse(kProbeDefaultUrl);
    if (!parsed.path.contains(kScheduleApiPath)) {
      return parsed;
    }
    return buildScheduleApiUrl(
      origin: '${parsed.scheme}://${parsed.host}',
      years: _yearsValue,
      week: 1,
      cardId: _cardIdValue,
    );
  }

  /// 页内取一次（带会话 / 匿名对照二选一）。**每轮最多 2 次**，有 10s 冷却。
  Future<void> _runInPage({required bool withCredentials}) async {
    if (!_canRequest) {
      return;
    }
    setState(() => _busy = true);
    final Future<InPageFetchSummary?> Function(String url, {required bool withCredentials})? fetch =
        widget.inPageFetch ?? SessionProbeJsBridge.inPageFetch;
    InPageFetchSummary? summary;
    if (fetch == null) {
      _notice = '当前没有可用的 WebView：请先打开「我的课表」再回来。';
    } else {
      summary = await fetch(_target().toString(), withCredentials: withCredentials);
    }
    if (!mounted) {
      return;
    }
    setState(() {
      if (withCredentials) {
        _withCookie = summary;
      } else {
        _anonymous = summary;
      }
      _requestsUsed += 1;
      _cooldownLeftSeconds = kCooldown.inSeconds;
      _busy = false;
    });
    _startCooldownTicker();
  }

  void _startCooldownTicker() {
    _cooldownTicker?.cancel();
    _cooldownTicker = Timer.periodic(const Duration(seconds: 1), (Timer timer) {
      if (!mounted) {
        timer.cancel();
        return;
      }
      setState(() {
        if (_cooldownLeftSeconds > 0) {
          _cooldownLeftSeconds -= 1;
        }
      });
      if (_cooldownLeftSeconds <= 0) {
        timer.cancel();
      }
    });
  }

  /// t142/t143：自动探测 cardId —— **纯本地 JS、零网络请求**。
  ///
  /// t143 修「按钮卡死」：改用 `evaluateJavascript` 的**直接返回值**（不走 JS→Dart 桥），
  /// 5s 超时 + try/catch，并且**无论成功/失败/超时都在 finally 里复位 `_probing`**，
  /// 失败时把明确错误显示出来（不再只是变灰）。
  Future<void> _probeCardIds() async {
    if (_probing) {
      return;
    }
    setState(() {
      _probing = true;
      _cardIdHintMessage = null;
      _cardIdHints = const <CardIdHint>[];
    });
    try {
      final ScriptEvaluator? evaluate = widget.evaluateScript ?? SessionProbeJsBridge.evaluate;
      if (evaluate == null) {
        _cardIdHintMessage = '当前没有可用的 WebView：请先打开「我的课表」再回来探测。';
        return;
      }
      final CardIdProbeOutcome outcome =
          await runCardIdProbe(evaluate: evaluate, timeout: widget.cardIdProbeTimeout);
      if (!mounted) {
        return;
      }
      setState(() {
        _cardIdHints = outcome.hints;
        _cardIdHintMessage = outcome.ok && outcome.hints.isNotEmpty ? null : outcome.message;
      });
    } on WebViewUnavailableException catch (failure) {
      // t146：控制器失效（MissingPluginException）→ 明确提示，而不是抛原生异常
      if (mounted) {
        setState(() => _cardIdHintMessage = '探测失败：${failure.message}');
      }
    } on Object catch (failure) {
      // 兜底：runCardIdProbe 自身不抛，这里防御未来改动
      if (mounted) {
        setState(() => _cardIdHintMessage = '探测失败：${failure.runtimeType}');
      }
    } finally {
      if (mounted) {
        setState(() => _probing = false);
      }
    }
  }

  void _useCardId(CardIdHint hint) {
    setState(() {
      _cardIdValue = hint.value;
      _cardIdHintMessage = '已填入 ${hint.masked}（来源 ${hint.source}）';
    });
  }

  /// 查 Cookie（t140：只查**目标 URL** 一个域，不做跨域扫描）。
  Future<void> _runDiagnostics() async {
    setState(() => _busy = true);
    final CookieDiagnostics diagnostics = await diagnoseCookies(
      target: _target(),
      source: widget.cookieRefSource ?? loadCampusCookieRefs,
      jsProbe: widget.jsProbe ?? SessionProbeJsBridge.documentCookieLength,
    );
    if (!mounted) {
      return;
    }
    setState(() {
      _diagnostics = diagnostics;
      _busy = false;
    });
  }

  /// CAS ticket 探针：**一次导航**（最多看 5 跳），15s 超时，可取消，结束自动回原页。
  Future<void> _runCasProbe() async {
    setState(() {
      _busy = true;
      _casRunning = true;
      _cas = null;
    });
    final String loginUrl = CasTicketProbe.loginUrl(
      casBase: _casBase.text,
      serviceUrl: _serviceUrl.text,
    );
    final Future<void> Function(String url)? navigate = widget.navigate ?? CasTicketProbe.navigate;
    if (navigate == null) {
      setState(() {
        _busy = false;
        _casRunning = false;
        _notice = '当前没有可用的 WebView：请先打开「我的课表」再回来试 CAS 探针。';
      });
      return;
    }

    final String origin = CasTicketProbe.returnUrl ?? kProbeDefaultUrl;
    CasTicketProbe.start();
    _casTicker?.cancel();
    _casTicker = Timer.periodic(const Duration(milliseconds: 250), (Timer timer) {
      if (!mounted) {
        timer.cancel();
        return;
      }
      setState(() {
        _cas = CasTicketResult.summarize(List<String>.of(CasTicketProbe.redirects));
      });
    });

    final Completer<void> done = Completer<void>();
    _casDone = done;
    final Timer timeout = Timer(kCasTimeout, () {
      if (!done.isCompleted) {
        done.complete();
      }
    });
    unawaited(navigate(loginUrl).whenComplete(() {
      if (!done.isCompleted) {
        done.complete();
      }
    }));

    try {
      await done.future;
    } finally {
      timeout.cancel();
      _casDone = null;
      CasTicketProbe.stop();
      _casTicker?.cancel();
      _casTicker = null;
      final List<String> chain = List<String>.of(CasTicketProbe.redirects);
      try {
        await navigate(origin);
      } on Object {
        // 回不去也不影响判读
      }
      if (!mounted) {
        return;
      }
      setState(() {
        _cas = CasTicketResult.summarize(chain);
        _busy = false;
        _casRunning = false;
      });
    }
  }

  void _cancelCasProbe() {
    final Completer<void>? done = _casDone;
    if (done != null && !done.isCompleted) {
      done.complete();
    }
    CasTicketProbe.stop();
    _casTicker?.cancel();
    _casTicker = null;
    setState(() {
      _cas = CasTicketResult.summarize(List<String>.of(CasTicketProbe.redirects));
      _busy = false;
      _casRunning = false;
    });
  }

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    return AlertDialog(
      title: const Text('[临时] 会话校验探针'),
      content: SizedBox(
        width: 560,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: <Widget>[
              Text(
                '临时诊断，验证后删除。校方域名有动态 JS 防护：请求一律在 WebView 内发，'
                '每轮最多 $kMaxRequestsPerRun 次、冷却 ${kCooldown.inSeconds}s。',
                style: theme.textTheme.bodySmall?.copyWith(color: theme.hintColor),
              ),
              const SizedBox(height: 8),
              _verdictBox(theme),
              // t146：删除「目标 URL / 学期 / cardId」手动输入（旧 Dart HTTP 入口已按 t140 移除）。
              // 现在只保留两件事：页内取一次（按钮）与 CAS 探针。
              Text(
                'cardId：${_cardIdValue.isEmpty ? '未探测（先点下面「自动探测 cardId」）' : maskCardIdValue(_cardIdValue)}'
                ' · 学期：$_yearsValue',
                key: const Key('probe-cardid-status'),
                style: theme.textTheme.bodySmall,
              ),
              // t142/t143：自动探测 cardId（页面内纯本地扫描，不发请求）
              Wrap(
                spacing: 8,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: <Widget>[
                  TextButton.icon(
                    key: const Key('probe-cardid-detect'),
                    onPressed: _probing ? null : _probeCardIds,
                    icon: _probing
                        ? const SizedBox(
                            width: 16,
                            height: 16,
                            child: CircularProgressIndicator(strokeWidth: 2),
                          )
                        : const Icon(Icons.search, size: 18),
                    label: Text(_probing ? '探测中…' : '自动探测 cardId'),
                  ),
                  const Text('需在课表页执行 · 纯本地扫描，不发请求',
                      key: Key('probe-cardid-hint'), style: TextStyle(fontSize: 11)),
                ],
              ),
              if (_cardIdHintMessage != null)
                Text(
                  _cardIdHintMessage!,
                  key: const Key('probe-cardid-message'),
                  style: theme.textTheme.bodySmall?.copyWith(color: theme.colorScheme.error),
                ),
              if (_cardIdHints.isNotEmpty) ...<Widget>[
                Text('候选（${_cardIdHints.length}）：', style: theme.textTheme.bodySmall),
                for (final CardIdHint hint in _cardIdHints)
                  Row(
                    children: <Widget>[
                      Expanded(
                        child: Text(
                          '${hint.masked}   ← ${hint.source}',
                          style: theme.textTheme.bodySmall,
                        ),
                      ),
                      TextButton(
                        onPressed: () => _useCardId(hint),
                        child: const Text('用这个'),
                      ),
                    ],
                  ),
              ],
              const SizedBox(height: 8),
              Wrap(
                spacing: 8,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: <Widget>[
                  FilledButton.tonalIcon(
                    key: const Key('probe-inpage-with-cookie'),
                    onPressed: _canRequest ? () => _runInPage(withCredentials: true) : null,
                    icon: const Icon(Icons.download_done, size: 18),
                    label: const Text('页内取一次（带会话）'),
                  ),
                  OutlinedButton.icon(
                    key: const Key('probe-inpage-anon'),
                    onPressed: _canRequest ? () => _runInPage(withCredentials: false) : null,
                    icon: const Icon(Icons.person_outline, size: 18),
                    label: const Text('匿名对照（1 次）'),
                  ),
                ],
              ),
              Text(
                '本轮已用请求：$_requestsUsed/$kMaxRequestsPerRun'
                '${_cooling ? ' · 冷却 ${_cooldownLeftSeconds}s 后可再点' : ''}',
                style: theme.textTheme.bodySmall,
              ),
              if (_withCookie != null) _runCard('带会话（页内）', _withCookie!, theme),
              if (_anonymous != null) _runCard('匿名对照（页内）', _anonymous!, theme),
              const SizedBox(height: 8),
              Wrap(
                spacing: 8,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: <Widget>[
                  OutlinedButton.icon(
                    key: const Key('probe-diagnose'),
                    onPressed: _busy ? null : _runDiagnostics,
                    icon: const Icon(Icons.bug_report_outlined, size: 18),
                    label: const Text('查 Cookie（目标 URL）'),
                  ),
                  if (_diagnostics != null)
                    Text(
                      '精确域 ${_diagnostics!.exact.length} 条 · '
                      'JS document.cookie 长度：${_diagnostics!.jsCookieLength ?? '拿不到'}',
                      style: theme.textTheme.bodySmall,
                    ),
                ],
              ),
              if (_diagnostics != null) ...<Widget>[
                Text(_diagnostics!.diagnosis, style: theme.textTheme.titleSmall),
                if (_diagnostics!.exact.isNotEmpty)
                  Text(describeCookieRefs(_diagnostics!.exact), style: theme.textTheme.bodySmall),
                const Divider(),
              ],
              if (_notice != null) Text(_notice!, style: theme.textTheme.bodySmall),
              Wrap(
                spacing: 8,
                children: <Widget>[
                  Text('Cookie：${_cookies.length} 条（不含内容）', style: theme.textTheme.bodySmall),
                  TextButton(onPressed: _busy ? null : _loadCookies, child: const Text('重新读取会话')),
                ],
              ),
              const SizedBox(height: 8),
              Text('CAS ticket 探针（一次导航）', style: theme.textTheme.titleSmall),
              TextField(
                key: const Key('probe-cas-base'),
                controller: _casBase,
                decoration: const InputDecoration(labelText: 'CAS 基址（如 https://ssoo.ynau.edu.cn）'),
              ),
              TextField(
                key: const Key('probe-service'),
                controller: _serviceUrl,
                decoration: const InputDecoration(labelText: 'service 地址（学校受保护地址）'),
              ),
              const SizedBox(height: 4),
              Wrap(
                spacing: 8,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: <Widget>[
                  OutlinedButton.icon(
                    key: const Key('probe-cas'),
                    onPressed: _busy ? null : _runCasProbe,
                    icon: const Icon(Icons.vpn_key_outlined, size: 18),
                    label: const Text('测试 CAS ticket'),
                  ),
                  if (_casRunning)
                    TextButton(
                      key: const Key('probe-cas-cancel'),
                      onPressed: _cancelCasProbe,
                      child: const Text('取消（保留已记录的跳转）'),
                    ),
                ],
              ),
              if (_casRunning)
                Text(
                  '进行中…已记录 ${CasTicketProbe.redirects.length} 跳'
                  '（${kCasTimeout.inSeconds}s 超时后自动结束并返回原页面）',
                  style: theme.textTheme.bodySmall,
                ),
              if (_cas != null) ...<Widget>[
                Text(
                  '是否拿到 ticket：${_cas!.ticketFound ? '是（ST-${_cas!.prefix}****）' : '否'} · '
                  '跳转 ${_cas!.hops} 次 · 终点域 ${_cas!.finalHost.isEmpty ? '—' : _cas!.finalHost}',
                  style: theme.textTheme.bodySmall,
                ),
                for (final String hop in _cas!.redirects.take(kCasMaxHops))
                  Text('· ${shortUrl(hop)}', style: theme.textTheme.bodySmall),
              ],
            ],
          ),
        ),
      ),
      actions: <Widget>[
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('关闭')),
      ],
    );
  }

  Widget _runCard(String label, InPageFetchSummary run, ThemeData theme) => Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: <Widget>[
          Padding(
            padding: const EdgeInsets.only(top: 10, bottom: 2),
            child: Text('$label：${run.verdict}', style: theme.textTheme.titleSmall),
          ),
          Text(
            '状态码 ${run.status} · 长度 ${run.length} · 含学号 ${run.hasStudentId ? '是' : '否'}'
            '${run.contentType.isEmpty ? '' : ' · ${run.contentType}'}',
            style: theme.textTheme.bodySmall,
          ),
        ],
      );

  /// 结论区：置顶显示（owner 反馈输出藏在下面看不到）。
  Widget _verdictBox(ThemeData theme) {
    if (_diagnostics == null && _withCookie == null && _anonymous == null && _cas == null) {
      return const SizedBox.shrink();
    }
    String? compare;
    if (_withCookie != null && _anonymous != null) {
      compare = (_withCookie!.status == _anonymous!.status &&
              _withCookie!.length == _anonymous!.length &&
              _withCookie!.hasStudentId == _anonymous!.hasStudentId)
          ? '❌ 带会话与匿名结果相同（公开页面或同一登录页）→ 本次结果无效'
          : '✅ 带会话与匿名结果不同 → 会话在 WebView 内确实生效';
    }
    return Container(
      key: const Key('probe-verdict-box'),
      width: double.infinity,
      margin: const EdgeInsets.only(bottom: 8),
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: theme.colorScheme.errorContainer.withValues(alpha: 0.35),
        borderRadius: BorderRadius.circular(8),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: <Widget>[
          if (_diagnostics != null)
            Text('Cookie：${_diagnostics!.diagnosis}', style: theme.textTheme.titleSmall),
          if (compare != null) ...<Widget>[
            const SizedBox(height: 4),
            Text(compare, style: theme.textTheme.titleSmall),
          ],
          if (_cas != null) ...<Widget>[
            const SizedBox(height: 4),
            Text('CAS：${_cas!.verdict}', style: theme.textTheme.titleSmall),
          ],
        ],
      ),
    );
  }
}
