// t150：抓取"等一下页面就绪"的轮询与就绪诊断。
//
// 上一轮的「固定 800ms 再取一次」没用 —— 因为问题常常不是"慢 800ms"，而是：
//   · WebView 还停在登录页/门户首页 → 拿到的永远是登录页 HTML（再等也一样）；
//   · 登录后跳转还没完成 → 抓到的是过渡页；
//   · 抓取完立刻关页面 → 第二次根本取不到新 DOM。
//
// 所以这里做两件事：
//   1. 每 [interval] 取一次（html + url + title），最多等到 [timeout]；
//   2. 每次记录**就绪诊断**（长度 / 标题 / URL / 是否登录页 / 是否含课表关键字），
//      失败消息里带全，下次真机失败一眼能看出是"登录页"还是"空表格"。

import 'package:flutter/foundation.dart';

/// 一次探测到的页面状态（不发网络请求，全部来自页面 DOM/URL）。
@immutable
class CaptureSnapshot {
  const CaptureSnapshot({
    required this.html,
    this.url = '',
    this.title = '',
    required this.tries,
    required this.atMs,
    this.rowCount = -1,
  });

  final String html;
  final String url;
  final String title;

  /// 第几次探测（1 = 首次）。
  final int tries;

  /// 从开始等待到这次探测的毫秒数。
  final int atMs;

  /// t151：课程行数（-1 = 不知道/没探测）。就绪判定要用它 ——
  /// "有课表框架但课程行还没渲染"是 PARSE_EMPTY 的真正中间态。
  final int rowCount;

  int get htmlLength => html.length;

  /// 页面像登录页/统一身份认证页（含关键字，或 URL 里有 login/cas/sso）。
  bool get looksLikeLoginPage {
    final String haystack = '$url $title $html';
    if (RegExp(r'登录|统一身份认证|CAS|login|signin', caseSensitive: false).hasMatch(haystack)) {
      return true;
    }
    return RegExp(r'/(login|cas|sso)', caseSensitive: false).hasMatch(url);
  }

  /// 是否含课表关键字。
  bool get hasTimetableMark =>
      RegExp(r'timetable|课程|节次|课表|周次', caseSensitive: false).hasMatch(html);

  /// 就绪 = 不是登录页 + 有课表关键字 + 长度够 + **课程行数 > 0**（t151）。
  ///
  /// [minRows] 默认 1：只要出现课程行就算有内容；探测不到行数（-1）时视为未就绪，
  /// 因为那正是"框架有了、行还没渲染"的状态（Rust 会解析出 0 门课 → PARSE_EMPTY）。
  bool isReady({int minLength = 1000, int minRows = 1}) =>
      !looksLikeLoginPage &&
      hasTimetableMark &&
      htmlLength >= minLength &&
      (minRows <= 0 || rowCount >= minRows);

  /// 诊断行（写进失败消息 / 日志，不含页面原文）。
  String get diagnostics =>
      '第 $tries 次（${atMs}ms）：html 长度=$htmlLength / 课程行数=${rowCount < 0 ? '未知' : rowCount} / '
      '标题="${_clip(title)}" / URL="${_clip(url)}" / 含登录字样=$looksLikeLoginPage / '
      '含课表关键字=$hasTimetableMark';

  static String _clip(String value) {
    if (value.isEmpty) {
      return '（空）';
    }
    return value.length <= 80 ? value : '${value.substring(0, 80)}…';
  }

  @override
  String toString() => 'CaptureSnapshot($diagnostics)';
}

/// 一次探测回调：返回页面 HTML（拿不到返回 null）。
typedef HtmlProbe = Future<String?> Function();

/// 一次"页面元信息"探测：URL 与标题（拿不到返回 null/空）。
typedef PageMetaProbe = Future<({String url, String title})?> Function();

/// t151：课程行数探测（页面侧轻量计数，不返回 DOM；拿不到返回 -1）。
typedef CourseRowProbe = Future<int> Function();

/// 等待页面就绪的结果。
@immutable
class CaptureWaitResult {
  const CaptureWaitResult({required this.snapshots, required this.last, required this.ready});

  /// 每次探测的快照（按时间顺序，最多 20 条以免消息过长）。
  final List<CaptureSnapshot> snapshots;
  final CaptureSnapshot last;

  /// 是否就绪（用**等待时**的阈值算出来的结果，避免结果对象再用默认阈值误判）。
  final bool ready;

  /// 失败消息：带上每一次的诊断（最多前 3 条 + 最后一条，避免刷屏）。
  String get failureMessage {
    final List<CaptureSnapshot> toShow = snapshots.length <= 4
        ? snapshots
        : <CaptureSnapshot>[...snapshots.take(3), snapshots.last];
    final String detail = toShow.map((CaptureSnapshot s) => s.diagnostics).join('；');
    if (last.looksLikeLoginPage) {
      return '还没登录/登录未完成（当前仍是登录页）→ 先在页面里完成登录：$detail';
    }
    if (!last.hasTimetableMark) {
      return '页面里找不到课表（可能不是课表页）：$detail';
    }
    if (last.rowCount >= 0 && last.rowCount < 1) {
      return '课表框架在，但**课程行还没渲染出来**（行数=${last.rowCount < 0 ? '未知' : last.rowCount}）'
          '→ 再等一会儿重试：$detail';
    }
    return '页面内容过短，可能还没渲染完：$detail';
  }
}

/// 轮询等待页面就绪；**不关页面、不发网络请求**（只读 DOM/URL）。
Future<CaptureWaitResult> waitForTimetableReady({
  required HtmlProbe html,
  PageMetaProbe? meta,
  CourseRowProbe? rows,
  Duration interval = const Duration(milliseconds: 500),
  Duration timeout = const Duration(seconds: 6),
  int minLength = 1000,
  int minRows = 1,
  bool Function()? isCancelled,
}) async {
  final Stopwatch watch = Stopwatch()..start();
  final List<CaptureSnapshot> snapshots = <CaptureSnapshot>[];
  CaptureSnapshot last = const CaptureSnapshot(html: '', tries: 0, atMs: 0);
  int tries = 0;
  while (true) {
    tries += 1;
    String? fetched;
    String url = '';
    String title = '';
    int rowCount = -1;
    try {
      fetched = await html();
      final ({String url, String title})? info = await meta?.call();
      url = info?.url ?? '';
      title = info?.title ?? '';
      if (rows != null) {
        rowCount = await rows();
      }
    } on Object {
      fetched ??= null;
    }
    last = CaptureSnapshot(
      html: fetched ?? '',
      url: url,
      title: title,
      tries: tries,
      atMs: watch.elapsedMilliseconds,
      rowCount: rowCount,
    );
    if (snapshots.length < 20) {
      snapshots.add(last);
    }
    if (last.isReady(minLength: minLength, minRows: rows == null ? 0 : minRows)) {
      return CaptureWaitResult(snapshots: snapshots, last: last, ready: true);
    }
    if (watch.elapsedMilliseconds >= timeout.inMilliseconds) {
      return CaptureWaitResult(snapshots: snapshots, last: last, ready: false);
    }
    if (isCancelled?.call() ?? false) {
      return CaptureWaitResult(snapshots: snapshots, last: last, ready: false);
    }
    await Future<void>.delayed(interval);
  }
}
/// 【t151】从 HTML 里数课程行（**纯本地、与 `kCourseRowSelector` 语义一致**：`table#timetable` 的 `<tr>`）。
///
/// 用途：PARSE_EMPTY 重试的诊断（`行数`）与测试；页面内则用 `window.__yunongCourseRowCount()`。
/// 表格行统计（t168 修复：区分**表头行**与**数据行**）。
///
/// 背景：`<tr>` 既可能是表头（只含 `<th>`）也可能是课程行（含 `<td>`）。
/// 旧实现把两者一起数，于是"只有表头"被算成 1 行、就绪判定放行，Rust 随后解析出 0 门课
/// —— 这就是 `PARSE_EMPTY` 的根因。
({int dataRows, int headerRows, int totalRows}) tableRowCounts(String html) {
  if (html.isEmpty) {
    return (dataRows: 0, headerRows: 0, totalRows: 0);
  }
  final RegExp tablePattern = RegExp(r'<table[^>]*id="timetable"[\s\S]*?</table>', caseSensitive: false);
  final RegExpMatch? table = tablePattern.firstMatch(html);
  final String scope = table?.group(0) ?? html;
  int data = 0;
  int header = 0;
  for (final RegExpMatch row in RegExp(r'<tr\b[\s\S]*?(?:</tr>|$)', caseSensitive: false).allMatches(scope)) {
    final String rowHtml = row.group(0) ?? '';
    if (RegExp(r'<td\b', caseSensitive: false).hasMatch(rowHtml)) {
      data += 1;
    } else if (RegExp(r'<th\b', caseSensitive: false).hasMatch(rowHtml)) {
      header += 1;
    }
  }
  return (dataRows: data, headerRows: header, totalRows: data + header);
}

/// 数据行数（= Rust 解析器会看的行；不含表头）。保留旧名字便于调用方迁移。
int countTableRowsInHtml(String html) => tableRowCounts(html).dataRows;