// 【临时诊断】会话校验探针 —— 验证后删除。
//
// # 目的（一次性前置验证，不是产品功能）
//
// 我们要判断：**拿到校方会话后，能否脱离 WebView、用纯 HTTP 请求打学校接口拿到 200**。
// 如果学校把会话绑定到浏览器特征（UA / 指纹 / JS 签名），云端"带令牌校验学号"的方案就不成立，
// 整个防刷模型要重做。这个探针就是为了在写任何云端代码之前把假设验证掉。
//
// # 安全约束
//
// - Cookie **不出设备**：不打印、不上传、不写文件，也不出现在任何 UI 文案里；
// - 结果只展示：状态码 / Content-Type / 响应体长度 / 是否命中 10~12 位数字 /
//   响应体前 200 字的**哈希**（非加密哈希，仅用于比对两个端点是否同一页面）/ 耗时 / 用的 UA；
// - 只发 **GET**，不跟随重定向（这样能看到 302 这类"被弹回登录页"的信号）。
//
// 依赖：只用 `dart:io` 的 HttpClient（不引入新依赖）。

import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// 一条待发送的 Cookie（值只在内存里，不进日志/结果）。
class ProbeCookie {
  const ProbeCookie(this.name, this.value);

  final String name;
  final String value;
}

/// 探针结果（**不含 Cookie、不含原始响应体**）。
class SessionProbeResult {
  const SessionProbeResult({
    required this.url,
    required this.userAgent,
    required this.statusCode,
    required this.contentType,
    required this.bodyLength,
    required this.studentIdHit,
    required this.studentIdMatchLength,
    required this.bodyHash,
    required this.elapsedMs,
    required this.cookieCount,
    this.error,
  });

  final String url;
  final String userAgent;
  final int statusCode;
  final String contentType;
  final int bodyLength;

  /// 是否命中"像学号"的 10~12 位数字（只给布尔，不给原文）。
  final bool studentIdHit;

  /// 命中的数字串长度（0 = 未命中）。
  final int studentIdMatchLength;

  /// 前 200 个字符的 FNV-1a 64 哈希（十六进制）——只用于比对两个端点是否同一页面。
  final String bodyHash;
  final int elapsedMs;

  /// 这次请求带了多少条 Cookie（数量，不含内容）。
  final int cookieCount;
  final String? error;

  bool get ok => error == null && statusCode >= 200 && statusCode < 300;

  /// 判读结论（给 owner 一眼看结论）。
  String get verdict {
    if (error != null) {
      return '❌ 请求失败：$error';
    }
    if (statusCode == 302 || statusCode == 301 || statusCode == 303 || statusCode == 307) {
      return '❌ 被重定向（多半弹回登录页）→ 会话可能绑定浏览器特征';
    }
    if (statusCode == 401 || statusCode == 403) {
      return '❌ 被拒绝（$statusCode）→ 会话不可脱离 WebView 使用';
    }
    if (statusCode >= 200 && statusCode < 300) {
      return studentIdHit
          ? '✅ 200 且命中 10~12 位数字 → 会话可脱离 WebView 使用，可能能找到身份端点'
          : '⚠️ 200 但没有 10~12 位数字 → 会话可用，但该端点不带身份信息';
    }
    return '⚠️ 未预期状态码：$statusCode';
  }

  /// 刻意只输出这些字段：**永远不包含 Cookie 值或响应体原文**。
  @override
  String toString() =>
      'SessionProbeResult(url=$url, ua=$userAgent, status=$statusCode, '
      'contentType=$contentType, len=$bodyLength, studentId=$studentIdHit/$studentIdMatchLength, '
      'hash=$bodyHash, ms=$elapsedMs, cookies=$cookieCount, error=$error)';
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

/// 会话域：默认探针目标（只读、无副作用）。
const String kProbeDefaultUrl = 'https://jwxt.ynau.edu.cn/DefaultStu';

/// 默认 UA：用 Flutter WebView2 的桌面 UA（探针默认值，可改）。
const String kProbeDefaultUserAgent =
    'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) '
    'Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0';

/// 学号正则：10~12 位连续数字（不展示原文）。
final RegExp kStudentIdPattern = RegExp(r'\b\d{10,12}\b');

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

/// 跑一次探针。
///
/// 注意：`cookies` 只用于拼 `Cookie` 请求头，**不会**出现在返回结果里。
Future<SessionProbeResult> runSessionProbe({
  required String url,
  required String userAgent,
  required List<ProbeCookie> cookies,
  ProbeClient? client,
  Duration timeout = const Duration(seconds: 15),
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

  final Map<String, String> headers = <String, String>{
    'User-Agent': userAgent,
    'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
    if (cookies.isNotEmpty)
      'Cookie': cookies.map((ProbeCookie c) => '${c.name}=${c.value}').join('; '),
  };

  final Stopwatch watch = Stopwatch()..start();
  try {
    final ProbeResponse response =
        await (client ?? IoProbeClient()).get(url: parsed, headers: headers, timeout: timeout);
    watch.stop();
    final RegExpMatch? match = kStudentIdPattern.firstMatch(response.body);
    return SessionProbeResult(
      url: parsed.toString(),
      userAgent: userAgent,
      statusCode: response.statusCode,
      contentType: response.contentType,
      bodyLength: response.body.length,
      studentIdHit: match != null,
      studentIdMatchLength: match?.group(0)?.length ?? 0,
      bodyHash: fnv1a64Hex(response.body),
      elapsedMs: watch.elapsedMilliseconds,
      cookieCount: cookies.length,
    );
  } on Object catch (failure) {
    watch.stop();
    return SessionProbeResult(
      url: parsed.toString(),
      userAgent: userAgent,
      statusCode: 0,
      contentType: '',
      bodyLength: 0,
      studentIdHit: false,
      studentIdMatchLength: 0,
      bodyHash: '',
      elapsedMs: watch.elapsedMilliseconds,
      cookieCount: cookies.length,
      // 只给异常类型，避免把含 Cookie 的请求细节带进 UI
      error: failure.runtimeType.toString(),
    );
  }
}

SessionProbeResult _errorResult(String url, String userAgent, int cookieCount, String error) =>
    SessionProbeResult(
      url: url,
      userAgent: userAgent,
      statusCode: 0,
      contentType: '',
      bodyLength: 0,
      studentIdHit: false,
      studentIdMatchLength: 0,
      bodyHash: '',
      elapsedMs: 0,
      cookieCount: cookieCount,
      error: error,
    );
