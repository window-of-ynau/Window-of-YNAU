// t167：抓取/解析失败的**诊断落盘**（让 PARSE_EMPTY 自己说清是哪一种）。
//
// 为什么需要它：owner 实测在新构建上仍报 `PARSE_EMPTY`，但界面只有错误码，
// 看不出是"行数为 0（选择器不对）"还是"行数 >0 但解析 0 门（解析器不匹配）"。
// 现在把每次尝试的结构特征 + HTML 首尾片段写进 `<数据根>/logs/`，并可一键打开目录。
//
// 脱敏：不写 Cookie、不写 token/ticket/authorization（见 [CaptureLog.sanitize]）。

import 'dart:io';

import 'package:flutter/foundation.dart';

import '../core/app_paths.dart';
import '../core/timetable_service.dart';

/// 一次抓取尝试的结构特征（不依赖 `ParseAttempt`，便于 UI 与日志复用）。
class CaptureAttemptRecord {
  const CaptureAttemptRecord({
    required this.index,
    required this.htmlLength,
    this.rowCount = -1,
    this.headerRowCount,
    this.title = '',
    this.url = '',
    this.looksLikeLoginPage = false,
    this.hasTimetableMark = false,
    this.parsedCourses,
  });

  final int index;
  final int htmlLength;
  /// 数据行数（含 `<td>` 的行；= Rust 解析器会看的行）。
  final int rowCount;

  /// 表头行数（只含 `<th>` 的行）；null = 没统计。
  final int? headerRowCount;
  final String title;
  final String url;
  final bool looksLikeLoginPage;
  final bool hasTimetableMark;

  /// 这次解析出几门课（null = 还没解析）。
  final int? parsedCourses;

  String get line =>
      '第 $index 次：html 长度=$htmlLength / '
      '表头行=${headerRowCount == null ? '未知' : headerRowCount} / '
      '数据行=${rowCount < 0 ? '未知' : rowCount} / '
      '标题="${title.trim()}" / URL="${url.trim()}" / 含登录字样=$looksLikeLoginPage / '
      '含课表关键字=$hasTimetableMark / 解析出=${parsedCourses == null ? '未解析' : '$parsedCourses 门课'}';
}

/// 抓取诊断日志（失败一次一文件；成功追加一行）。
class CaptureLog {
  const CaptureLog._();

  /// 日志目录：`<数据根>/logs`（与 SQLite/settings/secrets 同一个数据根）。
  static String logsDir() => <String>[AppPaths.dataDir, 'logs'].join(Platform.pathSeparator);

  /// 失败日志路径：`<logs>/capture-<yyyyMMdd-HHmmss>.log`。
  static String failurePath({DateTime? now}) {
    final DateTime at = now ?? DateTime.now();
    String two(int v) => v.toString().padLeft(2, '0');
    final String stamp = '${at.year}${two(at.month)}${two(at.day)}-'
        '${two(at.hour)}${two(at.minute)}${two(at.second)}';
    return <String>[logsDir(), 'capture-$stamp.log'].join(Platform.pathSeparator);
  }

  /// 成功记录：`<logs>/capture-success.log`（追加）。
  static String successPath() =>
      <String>[logsDir(), 'capture-success.log'].join(Platform.pathSeparator);

  /// 写失败日志，返回文件路径（写失败返回 null，不抛给 UI）。
  static String? writeFailure({
    required String code,
    required String html,
    required List<CaptureAttemptRecord> attempts,
    String userHint = '',
    String message = '',
    DateTime? now,
  }) {
    final DateTime at = now ?? DateTime.now();
    final String path = failurePath(now: at);
    final StringBuffer buffer = StringBuffer()
      ..writeln('# 云农之窗 · 抓取失败诊断')
      ..writeln('时间：${at.toIso8601String()}')
      ..writeln('错误码：$code')
      ..writeln('提示：${sanitize(userHint)}')
      ..writeln('原始消息：${sanitize(message)}')
      ..writeln('平台：${Platform.operatingSystem} ${Platform.operatingSystemVersion}')
      ..writeln('数据根：${AppPaths.dataDir}')
      ..writeln()
      ..writeln('## 每次尝试的结构特征')
      ..writeln('# 行数=0 → 选择器/表结构变了；行数>0 但 0 门 → 解析器与页面不匹配');
    for (final CaptureAttemptRecord attempt in attempts) {
      buffer.writeln(sanitize(attempt.line));
    }
    buffer
      ..writeln()
      ..writeln('## HTML 前 2000 字符')
      ..writeln(sanitize(_head(html)))
      ..writeln()
      ..writeln('## HTML 后 2000 字符')
      ..writeln(sanitize(_tail(html)));
    try {
      Directory(logsDir()).createSync(recursive: true);
      File(path).writeAsStringSync(buffer.toString(), flush: true);
      debugPrint('[capture] 诊断已写入 $path');
      return path;
    } on Object catch (failure) {
      debugPrint('[capture] 写诊断失败：${failure.runtimeType} $failure');
      return null;
    }
  }

  /// 成功时也记一行结构特征（便于和失败对比）。
  static void writeSuccess({
    required int htmlLength,
    required int rowCount,
    required String title,
    required String url,
    int? parsedCourses,
    DateTime? now,
  }) {
    final DateTime at = now ?? DateTime.now();
    final String line = '${at.toIso8601String()}  成功  '
        'html 长度=$htmlLength / 课程行数=$rowCount / '
        '解析出=${parsedCourses == null ? '未解析' : '$parsedCourses 门课'} / '
        '标题="${title.trim()}" / URL="${url.trim()}"';
    try {
      Directory(logsDir()).createSync(recursive: true);
      File(successPath()).writeAsStringSync(
        '${sanitize(line)}\n',
        mode: FileMode.append,
        flush: true,
      );
    } on Object catch (failure) {
      debugPrint('[capture] 写成功记录失败：${failure.runtimeType} $failure');
    }
  }

  /// 打开日志目录所在的文件管理器。
  ///
  /// - Windows：`explorer <dir>`；macOS：`open <dir>`；Linux：`xdg-open <dir>`；
  /// - Android/iOS：没有通用的"打开目录"入口 → 返回 false（UI 显示路径让用户自行复制）。
  static Future<bool> openLogsDirectory() async {
    final String dir = logsDir();
    try {
      Directory(dir).createSync(recursive: true);
    } on Object {
      // 目录建不出来也继续尝试打开
    }
    final List<String>? command = openCommandFor(Platform.operatingSystem, dir);
    if (command == null) {
      return false;
    }
    try {
      await Process.start(
        command.first,
        command.sublist(1),
        mode: ProcessStartMode.detached,
      );
      return true;
    } on Object catch (failure) {
      debugPrint('[capture] 打开日志目录失败：${failure.runtimeType} $failure');
      return false;
    }
  }

  /// 平台分支（纯函数，便于测试）。
  static List<String>? openCommandFor(String operatingSystem, String dir) {
    switch (operatingSystem) {
      case 'windows':
        return <String>['explorer', dir];
      case 'macos':
        return <String>['open', dir];
      case 'linux':
        return <String>['xdg-open', dir];
      default:
        return null; // Android / iOS：没有通用入口
    }
  }

  /// 脱敏：去掉 Cookie / Set-Cookie / token / ticket / authorization 的值。
  static String sanitize(String text) {
    if (text.isEmpty) {
      return text;
    }
    String out = text;
    // Dart 的 RegExp 不支持内联 `(?i)`，用 caseSensitive: false。
    out = out.replaceAll(
      RegExp(r'(cookie|set-cookie)\s*[:=]\s*[^\r\n]*', caseSensitive: false),
      r'$1: <已脱敏>',
    );
    out = out.replaceAll(
      RegExp(
        r'\b(token|ticket|authorization|access_token|id_token|password)\b\s*[:=]\s*[^\s;&,"<>]+',
        caseSensitive: false,
      ),
      r'$1=<已脱敏>',
    );
    return out;
  }

  static String _head(String html) => html.length <= 2000 ? html : html.substring(0, 2000);

  static String _tail(String html) => html.length <= 2000 ? '' : html.substring(html.length - 2000);
}

/// 失败提示的**短消息**（详情放在可展开面板与日志里）。
String captureFailureHeadline(String code) {
  switch (code) {
    case 'PARSE_EMPTY':
      return '抓取失败：页面里没找到课程（详见日志）';
    case 'NOT_LOGGED_IN':
      return '抓取失败：登录还没完成（详见日志）';
    case 'HTML_CHANGED':
      return '抓取失败：页面结构变了（详见日志）';
    case 'BRIDGE_UNAVAILABLE':
      return '抓取失败：内嵌页面通道没建立起来（详见日志）';
    case 'CAPTURE_TIMEOUT':
      return '抓取超时：请在页面里打开课表卡片后重试（详见日志）';
    default:
      return '';
  }
}

/// 最近 [take] 次尝试的诊断行（给可展开面板用）。
List<String> recentAttemptLines(List<ParseAttempt> attempts, {int take = 3}) {
  final List<ParseAttempt> tail =
      attempts.length <= take ? attempts : attempts.sublist(attempts.length - take);
  return tail.map((ParseAttempt a) => a.diagnostics).toList();
}