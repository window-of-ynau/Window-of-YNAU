// 【临时诊断】会话校验探针 —— 验证后删除。
//
// # 目的（一次性前置验证，不是产品功能）
//
// 判断：**拿到校方会话后，能否脱离 WebView、用纯 HTTP 请求打学校"需要登录"的接口拿到 200**。
// 如果学校把会话绑定到浏览器特征（UA / 指纹 / JS 签名），云端"带令牌校验学号"的方案就不成立。
//
// # t136 修正（第一版判读有缺陷，owner 实测拿到无效证据）
//
// owner 实际跑出「Cookie 0 条 / 200 / 命中学号」—— 说明目标 URL 是**公开页面**，结论不可信。
// 因此本版：
//   1. Cookie 为空时**直接不给 ✅**，只给「未带会话，不能用于判断」；
//   2. 每次运行做**匿名对照**（同一 URL 先带 Cookie、再不带），两次哈希相同 → 公开页面/登录页 → 无效；
//   3. 学号只给**脱敏片段**（前2 + *** + 后2），并提取 `<title>` 与是否含「登录/统一身份认证」；
//   4. 结论句式直白（见 [SessionProbeResult.verdict]）。
//
// # 安全约束
//
// - Cookie **不出设备**：不打印、不上传、不写文件，也不出现在任何 UI 文案里（只显示条数）；
// - 匿名对照组**不带任何会话信息**（不带 Cookie 头）；
// - 只发 **GET**，不跟随重定向（能看到 302）。

import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// 一条待发送的 Cookie（值只在内存里，不进日志/结果）。
class ProbeCookie {
  const ProbeCookie(this.name, this.value);

  final String name;
  final String value;
}

/// 单次请求的观测结果（**不含 Cookie、不含原始响应体**）。
class ProbeRun {
  const ProbeRun({
    required this.statusCode,
    required this.contentType,
    required this.bodyLength,
    required this.bodyHash,
    required this.elapsedMs,
    this.studentIdMasked,
    this.title = '',
    this.loginHint = false,
    this.bodySnippet,
    this.error,
  });

  final int statusCode;
  final String contentType;
  final int bodyLength;
  final String bodyHash;
  final int elapsedMs;

  /// 脱敏后的学号片段：`20***56`（未命中 = null）。
  final String? studentIdMasked;

  /// `<title>` 文本（截断到 80 字）。
  final String title;

  /// 页面里出现「登录」/「统一身份认证」等字样（几乎一定说明会话无效）。
  final bool loginHint;

  /// 【t139】非 2xx 且 Content-Type 含 json 时，响应体前 300 字符。
  ///
  /// 只对 JSON 生效：接口错误信息不敏感，而且**是判断"请求哪里不合法"的唯一线索**；
  /// HTML 一律不展示正文（避免整页内容泄露）。
  final String? bodySnippet;

  final String? error;

  bool get ok => error == null && statusCode >= 200 && statusCode < 300;

  @override
  String toString() => 'ProbeRun(status=$statusCode, len=$bodyLength, hash=$bodyHash, '
      'sid=$studentIdMasked, title="$title", login=$loginHint, ms=$elapsedMs, error=$error)';
}

/// 一次探针运行 = 带 Cookie + 匿名对照（同一 URL）。
class SessionProbeResult {
  const SessionProbeResult({
    required this.url,
    required this.userAgent,
    required this.cookieCount,
    required this.withCookie,
    required this.anonymous,
  });

  final String url;
  final String userAgent;
  final int cookieCount;
  final ProbeRun withCookie;
  final ProbeRun anonymous;

  /// 两次响应是否同一页面（公开页面 / 同一张登录页）。
  bool get sameAsAnonymous =>
      withCookie.bodyHash.isNotEmpty && withCookie.bodyHash == anonymous.bodyHash;

  /// 结论（t136 判定表，按顺序命中即返回）。
  String get verdict {
    // 1) 没带会话：任何"看起来 OK"的结果都不可用于判断
    if (cookieCount == 0) {
      return '⚠️ 未带会话：本次结果**不能**用于判断会话可用性（请先在校务 WebView 里登录后重试）';
    }
    if (withCookie.error != null) {
      return '❌ 请求失败（${withCookie.error}）：换 URL 或检查网络后再试';
    }
    // 2) 被重定向 / 拒绝 → 会话不能脱离 WebView 使用
    const Set<int> redirects = <int>{301, 302, 303, 307, 308};
    if (redirects.contains(withCookie.statusCode)) {
      return '⚠️ 会话无效：带 Cookie 被重定向（${withCookie.statusCode}）→ 多半弹回登录页';
    }
    if (withCookie.statusCode == 401 || withCookie.statusCode == 403) {
      return '❌ 会话无效：带 Cookie 被拒绝（${withCookie.statusCode}）';
    }
    if (!withCookie.ok) {
      return '⚠️ 未预期状态码：${withCookie.statusCode}';
    }
    // 3) 对照组：内容与匿名一致 → 公开页面/登录页，结果无效
    if (sameAsAnonymous) {
      return '❌ 会话无效：带 Cookie 与不带 Cookie 返回相同（公开页面或登录页）→ 请换一个需要登录的接口';
    }
    // 4) 200 且内容不同，但正文像登录页 → 仍判定无效
    if (withCookie.loginHint) {
      final String title = withCookie.title.isEmpty ? '无标题' : withCookie.title;
      return '⚠️ 会话无效：返回内容含「登录/统一身份认证」字样（$title）';
    }
    // 5) 200 + 内容不同 → 会话可用；有学号片段才算能验证身份
    if (withCookie.studentIdMasked != null) {
      return '✅ 会话有效：带 Cookie 返回 200 且内容不同，并命中疑似学号 ${withCookie.studentIdMasked}';
    }
    return '✅ 会话有效：带 Cookie 返回 200 且内容不同（但该端点没有学号线索，无法验证“令牌属于该学号”）';
  }

  /// 刻意只输出这些字段：**永远不包含 Cookie 值**。
  @override
  String toString() => 'SessionProbeResult(url=$url, ua=$userAgent, cookies=$cookieCount, '
      'with=$withCookie, anon=$anonymous, same=$sameAsAnonymous)';
}

/// HTTP 客户端抽象（测试注入假实现；默认走 dart:io HttpClient）。
abstract class ProbeClient {
  Future<ProbeResponse> get({
    required Uri url,
    required Map<String, String> headers,
    required Duration timeout,
  });
}

class ProbeResponse {
  const ProbeResponse({
    required this.statusCode,
    required this.contentType,
    required this.body,
  });

  final int statusCode;
  final String contentType;
  final String body;
}

/// 默认实现：GET、**不跟随重定向**（能观察到 302）。
class IoProbeClient implements ProbeClient {
  @override
  Future<ProbeResponse> get({
    required Uri url,
    required Map<String, String> headers,
    required Duration timeout,
  }) async {
    final HttpClient client = HttpClient()..connectionTimeout = timeout;
    try {
      final HttpClientRequest request = await client.getUrl(url).timeout(timeout);
      request.followRedirects = false;
      headers.forEach(request.headers.set);
      final HttpClientResponse response = await request.close().timeout(timeout);
      final String body = await response.transform(utf8.decoder).join().timeout(timeout);
      return ProbeResponse(
        statusCode: response.statusCode,
        contentType: response.headers.contentType?.toString() ?? '',
        body: body,
      );
    } finally {
      client.close(force: true);
    }
  }
}

/// 默认目标：**需要登录**的课表数据接口（只读）。
///
/// 来源：`portal_api.dart` 的 `kScheduleApiPath = '/api/uppcard/kbsz/queryAWeekSchedule'`
/// —— 门户页登录后才可由脚本调用；未登录会 302/401，而不是返回数据。
const String kProbeDefaultUrl =
    'https://one.ynau.edu.cn/api/uppcard/kbsz/queryAWeekSchedule?years=2026-2027&results=1';

/// 默认 UA：桌面 WebView2 的 UA（可改，用来判断有无 UA 校验）。
const String kProbeDefaultUserAgent =
    'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) '
    'Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0';

/// 学号正则：10~12 位连续数字（只展示脱敏片段）。
final RegExp kStudentIdPattern = RegExp(r'\b\d{10,12}\b');
final RegExp _titlePattern = RegExp(r'<title[^>]*>(.*?)</title>', dotAll: true, caseSensitive: false);
final RegExp _loginPattern = RegExp(r'登录|统一身份认证|请先登录|login', caseSensitive: false);

/// 学号脱敏：`2023123456` → `20***56`。
String maskStudentId(String raw) {
  if (raw.length <= 4) {
    return '**';
  }
  return '${raw.substring(0, 2)}***${raw.substring(raw.length - 2)}';
}

/// FNV-1a 64 位（非加密；仅用于"两个端点是不是同一张页面"的比对）。
String fnv1a64Hex(String text) {
  const int offsetBasis = 0xcbf29ce484222325;
  const int prime = 0x100000001b3;
  int hash = offsetBasis;
  final List<int> bytes = utf8.encode(text.length > 200 ? text.substring(0, 200) : text);
  for (final int byte in bytes) {
    hash ^= byte;
    hash = (hash * prime) & 0xFFFFFFFFFFFFFFFF;
  }
  return hash.toUnsigned(64).toRadixString(16).padLeft(16, '0');
}

ProbeRun _toRun(ProbeResponse response, int elapsedMs) {
  final String body = response.body;
  final RegExpMatch? sid = kStudentIdPattern.firstMatch(body);
  final String rawTitle = _titlePattern.firstMatch(body)?.group(1) ?? '';
  final String title = rawTitle.replaceAll(RegExp(r'\s+'), ' ').trim();
  return ProbeRun(
    statusCode: response.statusCode,
    contentType: response.contentType,
    bodyLength: body.length,
    bodyHash: fnv1a64Hex(body),
    elapsedMs: elapsedMs,
    studentIdMasked: sid == null ? null : maskStudentId(sid.group(0) ?? ''),
    title: title.length > 80 ? title.substring(0, 80) : title,
    loginHint: _loginPattern.hasMatch(body) || _loginPattern.hasMatch(title),
    bodySnippet: jsonErrorSnippet(body, response.contentType, response.statusCode),
  );
}

ProbeRun _errorRun(int elapsedMs, String error) => ProbeRun(
      statusCode: 0,
      contentType: '',
      bodyLength: 0,
      bodyHash: '',
      elapsedMs: elapsedMs,
      error: error,
    );

/// 请求头档位（t138：412 很可能是反爬/WAF，先看"纯粹差请求头"是否成立）。
enum ProbeHeaderMode {
  /// 裸请求：只带 UA 与 Accept。
  bare('裸请求'),

  /// 浏览器化请求头（UA 用 WebView 的桌面 UA）。
  browser('加浏览器头'),

  /// 浏览器化请求头 + 与 WebView 同 UA（本档的 UA 与 browser 档一致，便于单独对比）。
  browserWithUa('加浏览器头 + 同 UA');

  const ProbeHeaderMode(this.label);

  final String label;
}

/// 浏览器化请求头（t138）。
Map<String, String> browserLikeHeaders({
  required String userAgent,
  required Uri target,
  required ProbeHeaderMode mode,
}) {
  if (mode == ProbeHeaderMode.bare) {
    return <String, String>{
      'User-Agent': userAgent,
      'Accept': 'text/html,application/json,*/*;q=0.8',
    };
  }
  final String origin = '${target.scheme}://${target.host}';
  return <String, String>{
    'User-Agent': userAgent,
    'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,'
        'image/avif,image/webp,*/*;q=0.8',
    'Accept-Language': 'zh-CN,zh;q=0.9,en;q=0.8',
    'Accept-Encoding': 'gzip, deflate, br',
    'Referer': '$origin/',
    'Origin': origin,
    'Sec-Fetch-Dest': 'empty',
    'Sec-Fetch-Mode': 'cors',
    'Sec-Fetch-Site': 'same-origin',
    'Sec-Ch-Ua': '"Chromium";v="120", "Microsoft Edge";v="120", "Not-A.Brand";v="99"',
    'Sec-Ch-Ua-Mobile': '?0',
    'Sec-Ch-Ua-Platform': '"Windows"',
    'Connection': 'keep-alive',
  };
}

/// 一组"请求头档位"的观测结果。
class HeaderTrial {
  const HeaderTrial({required this.mode, required this.run});

  final ProbeHeaderMode mode;
  final ProbeRun run;

  String get label => mode.label;
}

/// 【t139】非 2xx 且 Content-Type 是 JSON 时，取响应体前 [max] 字符。
///
/// HTML 一律返回 null（不展示正文）；成功响应也不返回（没必要，且减少泄露面）。
String? jsonErrorSnippet(String body, String contentType, int statusCode, {int max = 300}) {
  final bool isJson = contentType.toLowerCase().contains('json');
  final bool failed = statusCode < 200 || statusCode >= 300;
  if (!isJson || !failed || body.isEmpty) {
    return null;
  }
  return body.length > max ? body.substring(0, max) : body;
}

/// 三组对照的判读（t138）。
String headerMatrixVerdict(List<HeaderTrial> trials) {
  if (trials.isEmpty) {
    return '尚未运行';
  }
  final HeaderTrial bare = trials.first;
  final HeaderTrial browser = trials.length > 1 ? trials[1] : bare;
  final HeaderTrial withUa = trials.length > 2 ? trials[2] : browser;
  final bool bareBlocked = bare.run.statusCode == 412;
  final bool browserOk = browser.run.ok;
  final bool browserNeedsAuth = browser.run.statusCode == 401 ||
      browser.run.statusCode == 403 ||
      browser.run.statusCode == 302 ||
      browser.run.statusCode == 301;

  if (browserOk) {
    return '✅ 只差请求头：裸请求 ${bare.run.statusCode} → 加浏览器头 200，'
        '说明反爬只校验请求头，云端仍有机会（但需长期维护头集合）';
  }
  if (bareBlocked && browserNeedsAuth) {
    return '⚠️ 反爬过了但仍需登录：裸请求 412 → 加浏览器头 ${browser.run.statusCode}，'
        '说明还差有效会话（Cookie/令牌）';
  }
  if (browser.run.statusCode == 412 && withUa.run.statusCode == 412) {
    return '❌ 仍是 412：加浏览器头（含同 UA）也被拦截 → 反爬不止校验请求头'
        '（指纹 / JS 挑战 / TLS 特征），纯 HTTP 方案基本不可行';
  }
  return '⚠️ 结果混合：裸 ${bare.run.statusCode} / 加头 ${browser.run.statusCode} / '
      '加头同UA ${withUa.run.statusCode} —— 请把三行一起判读';
}

/// 跑三组请求头对照（同一 URL、同一 Cookie）。
Future<List<HeaderTrial>> runHeaderMatrix({
  required String url,
  required String userAgent,
  required List<ProbeCookie> cookies,
  ProbeClient? client,
  Duration timeout = const Duration(seconds: 15),
}) async {
  final Uri parsed;
  try {
    parsed = Uri.parse(url.trim());
  } on FormatException {
    return const <HeaderTrial>[];
  }
  final ProbeClient probeClient = client ?? IoProbeClient();
  final List<HeaderTrial> trials = <HeaderTrial>[];
  for (final ProbeHeaderMode mode in ProbeHeaderMode.values) {
    final Map<String, String> headers = browserLikeHeaders(
      // 裸请求档故意不用浏览器 UA：先看"最坏情况"
      userAgent: mode == ProbeHeaderMode.bare ? 'Dart/HttpClient' : userAgent,
      target: parsed,
      mode: mode,
    );
    if (cookies.isNotEmpty) {
      headers['Cookie'] = cookies.map((ProbeCookie c) => '${c.name}=${c.value}').join('; ');
    }
    final Stopwatch watch = Stopwatch()..start();
    try {
      final ProbeResponse response =
          await probeClient.get(url: parsed, headers: headers, timeout: timeout);
      trials.add(HeaderTrial(mode: mode, run: _toRun(response, watch.elapsedMilliseconds)));
    } on Object catch (failure) {
      trials.add(HeaderTrial(
        mode: mode,
        run: _errorRun(watch.elapsedMilliseconds, failure.runtimeType.toString()),
      ));
    }
    watch.stop();
  }
  return trials;
}

/// 跑一次探针：**同一 URL 先带 Cookie、再匿名对照**。
///
/// `cookies` 只用于拼 `Cookie` 请求头，**不会**出现在返回结果里；匿名请求不带任何会话信息。
Future<SessionProbeResult> runSessionProbe({
  required String url,
  required String userAgent,
  required List<ProbeCookie> cookies,
  ProbeClient? client,
  Duration timeout = const Duration(seconds: 15),
  /// t139：Accept 头。对齐页内抓取请传 `kScheduleAcceptHeader`。
  String accept = 'text/html,application/json,application/xhtml+xml,*/*;q=0.8',
}) async {
  final Uri parsed;
  try {
    parsed = Uri.parse(url.trim());
  } on FormatException catch (failure) {
    return _errorResult(url, userAgent, cookies.length, 'URL 非法：${failure.message}');
  }
  if (!parsed.hasScheme || (parsed.scheme != 'http' && parsed.scheme != 'https')) {
    return _errorResult(url, userAgent, cookies.length, 'URL 必须是 http(s)');
  }

  final ProbeClient probeClient = client ?? IoProbeClient();
  final Map<String, String> baseHeaders = <String, String>{
    'User-Agent': userAgent,
    // t139：页内抓取用的就是这份 Accept；对齐后服务端不会再因为"形状不对"报 500。
    'Accept': accept,
  };

  // ① 带 Cookie
  final Stopwatch withWatch = Stopwatch()..start();
  ProbeRun withCookie;
  try {
    withCookie = _toRun(
      await probeClient.get(
        url: parsed,
        headers: <String, String>{
          ...baseHeaders,
          if (cookies.isNotEmpty)
            'Cookie': cookies.map((ProbeCookie c) => '${c.name}=${c.value}').join('; '),
        },
        timeout: timeout,
      ),
      withWatch.elapsedMilliseconds,
    );
  } on Object catch (failure) {
    withCookie = _errorRun(withWatch.elapsedMilliseconds, failure.runtimeType.toString());
  }
  withWatch.stop();

  // ② 匿名对照（不带 Cookie）
  final Stopwatch anonWatch = Stopwatch()..start();
  ProbeRun anonymous;
  try {
    anonymous = _toRun(
      await probeClient.get(url: parsed, headers: baseHeaders, timeout: timeout),
      anonWatch.elapsedMilliseconds,
    );
  } on Object catch (failure) {
    anonymous = _errorRun(anonWatch.elapsedMilliseconds, failure.runtimeType.toString());
  }
  anonWatch.stop();

  return SessionProbeResult(
    url: parsed.toString(),
    userAgent: userAgent,
    cookieCount: cookies.length,
    withCookie: withCookie,
    anonymous: anonymous,
  );
}

SessionProbeResult _errorResult(String url, String userAgent, int cookieCount, String error) =>
    SessionProbeResult(
      url: url,
      userAgent: userAgent,
      cookieCount: cookieCount,
      withCookie: _errorRun(0, error),
      anonymous: _errorRun(0, error),
    );
