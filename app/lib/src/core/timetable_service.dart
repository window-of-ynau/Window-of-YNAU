// 课表领域模型 + 服务层 + 校务取数接口。
//
// 分层：
//   SchoolFetcher（WebView/调试导入）→ 页面 HTML
//        ↓
//   TimetableService（这里）：调 Rust 核心（解析 + SQLite + 查询）
//
// 隐私（ADR-02 / ADR-11）：这里**不碰 Cookie**，也不打印 HTML；
// 只把 HTML 交给 Rust 侧解析，落库的是结构化课程。

import 'dart:convert';
import 'dart:io';

import 'package:flutter/services.dart' show rootBundle;

import '../campus/capture_cache.dart';
import '../campus/portal_api.dart';
import 'ffi_bindings.dart';

/// 一门已入库的课。
class StoredCourse {
  const StoredCourse({
    required this.id,
    required this.term,
    required this.name,
    required this.weekday,
    required this.startPeriod,
    required this.endPeriod,
    required this.weeks,
    this.room,
    this.teacher,
  });

  final int id;
  final String term;
  final String name;

  /// 1 = 周一 … 7 = 周日
  final int weekday;
  final int startPeriod;
  final int endPeriod;
  final String weeks;
  final String? room;
  final String? teacher;

  factory StoredCourse.fromJson(Map<String, dynamic> json) => StoredCourse(
        id: json['id'] as int? ?? 0,
        term: json['term'] as String? ?? '',
        name: json['name'] as String? ?? '',
        weekday: json['weekday'] as int? ?? 1,
        startPeriod: json['start_period'] as int? ?? 0,
        endPeriod: json['end_period'] as int? ?? 0,
        weeks: json['weeks'] as String? ?? '',
        room: json['room'] as String?,
        teacher: json['teacher'] as String?,
      );

  String get weekdayLabel =>
      const <String>['一', '二', '三', '四', '五', '六', '日'][(weekday - 1).clamp(0, 6)];

  String get periodLabel => '第 $startPeriod-$endPeriod 节';
}

/// 一次导入的结果。
class ImportOutcome {
  const ImportOutcome({
    required this.term,
    required this.imported,
    required this.warnings,
  });

  final String term;
  final int imported;

  /// 解析告警（"解析不完整，请核对后反馈"），不影响已入库课程。
  final List<String> warnings;
}

/// 门户周数据入库的结果（W6）。
class ScheduleImportOutcome {
  const ScheduleImportOutcome({
    required this.term,
    required this.imported,
    required this.warnings,
    required this.weeksSeen,
    required this.missingWeeks,
    required this.partial,
    this.weekcount,
  });

  final String term;
  final int imported;
  final List<String> warnings;
  final List<int> weeksSeen;
  final List<int> missingWeeks;

  /// 部分周没拉到但仍入库（不整体失败）
  final bool partial;
  final int? weekcount;
}

/// 一次 ICS 导出的结果。
class IcsExportResult {
  const IcsExportResult({required this.path, required this.events});

  final String path;

  /// 展开后的独立事件数（= 课程 × 周次；0 表示这学期还没排课）
  final int events;
}

/// 导出文件名（纯函数，便于单测）：`云农之窗-课表-<term>.ics`。
String icsFileName(String term) => '云农之窗-课表-$term.ics';

/// 不静默覆盖：目标已存在时依次尝试 `… (2).ics`、`… (3).ics`（最多到 99）。
///
/// `exists` 由调用方注入（生产用 `File(...).existsSync()`），单测可以完全离线。
String uniqueIcsPath({
  required String directory,
  required String fileName,
  required bool Function(String path) exists,
  int maxAttempts = 99,
}) {
  final separator = directory.endsWith('/') || directory.endsWith(r'\') ? '' : Platform.pathSeparator;
  final base = fileName.endsWith('.ics') ? fileName.substring(0, fileName.length - 4) : fileName;
  var candidate = '$directory$separator$fileName';
  var attempt = 2;
  while (exists(candidate)) {
    if (attempt > maxAttempts) {
      throw const TimetableException(
        code: 'EXPORT_PATH_BUSY',
        userHint: '导出目录里同名文件太多了，请清理后重试',
        message: 'too many existing ics files',
      );
    }
    candidate = '$directory$separator$base ($attempt).ics';
    attempt += 1;
  }
  return candidate;
}
/// 学期一条记录（`starts_on` 是用户确认过的开学日，空串=未确认）。
class TermSummary {
  const TermSummary({required this.term, required this.startsOn});

  final String term;
  final String startsOn;

  bool get hasStart => startsOn.isNotEmpty;
}

/// 课表相关的用户可见失败。
///
/// `userHint` 是**大字号给用户看的那句**；`message` 是原始细节（小字/日志）。
class TimetableException implements Exception {
  const TimetableException({
    required this.code,
    required this.userHint,
    required this.message,
  });

  final String code;
  final String userHint;
  final String message;

  @override
  String toString() => 'TimetableException($code): $message';
}

/// 从"错误码 + 原始信息"生成用户可见提示。
///
/// 这条映射是全 app 唯一的：**解析失败绝不能显示成空课表**。
TimetableException describeFailure(String? code, String? message) {
  final detail = message?.trim().isNotEmpty == true ? message! : '未知原因';
  switch (code) {
    case 'PARSE_FAILED':
      return TimetableException(
        code: code!,
        userHint: '学校页面可能已更新，请反馈',
        message: detail,
      );
    case 'CARD_NOT_FOUND':
      return TimetableException(
        code: code!,
        userHint: '没抓到课表接口：请先在门户里打开一次课表卡片',
        message: detail,
      );
    case 'AUTH_REQUIRED':
      return TimetableException(
        code: code!,
        userHint: '登录态已失效，请重新登录',
        message: detail,
      );
    case 'TERM_START_UNKNOWN':
      return TimetableException(
        code: code!,
        userHint: '本学期的第 1 周日期还没确认，无法换算具体日期',
        message: detail,
      );
    case 'PARSE_EMPTY':
      return TimetableException(
        code: code!,
        userHint: '未解析到课程：请确认本学期是否已排课',
        message: detail,
      );
    case 'BAD_INPUT':
      return TimetableException(
        code: code!,
        userHint: '内容为空，请重新抓取',
        message: detail,
      );
    case 'STORAGE':
      return TimetableException(
        code: code!,
        userHint: '本地课表保存失败',
        message: detail,
      );
    default:
      return TimetableException(
        code: code ?? 'INTERNAL',
        userHint: '内部错误，请稍后重试',
        message: detail,
      );
  }
}

/// 校务页面取数入口（WebView 版 / 调试导入都实现它）。
abstract class SchoolFetcher {
  /// 打开校务入口、等用户登录、抓取课表页 **HTML 原文**。
  Future<FetchedPage> fetchTimetablePage({required String entryUrl});

  /// W6 新形态：直接取门户周数据（默认不支持 → 调用方退回 HTML 路径）。
  Future<ScheduleApiResult> fetchSchedule({required String entryUrl, String? term}) =>
      Future<ScheduleApiResult>.error(
        UnsupportedError('该取数实现不支持门户接口，请退回 HTML 路径'),
      );

  /// 这个实现当前能不能用（Windows 上 WebView 未初始化时 UI 要给出解释）。
  bool get isAvailable;

  /// 不可用时的原因（直接显示给用户）。
  String? get unavailableReason;
}

/// 一次抓取的结果。
class FetchedPage {
  const FetchedPage({required this.html, required this.url});

  final String html;
  final String url;
}

/// 取数失败（未登录 / 页面改版 / 用户取消）。
class FetchException implements Exception {
  const FetchException({required this.code, required this.userHint, this.message});

  final String code;
  final String userHint;
  final String? message;

  @override
  String toString() => 'FetchException($code): ${message ?? userHint}';
}

/// 【t146】抓取重试：HTML 太短 / 不含课表关键字时，**间隔 [delay] 自动重试一次**。
///
/// 背景：每次构建后第一次抓取必报 `PARSE_EMPTY`，重试就正常 —— 首次构建后 WebView 缓存为空、
/// 页面渲染更慢，抓取窗口里拿到的是空壳 HTML。按内容判断 + 重试一次，比固定延时靠谱。
///
/// - [minLength]：低于它视为"空壳"（默认 2000 字符；真实课表页远大于此）；
/// - 第二次仍不合格就**返回最后一次结果**，由上游照常报错（不掩盖问题）。
Future<HtmlFetchAttempt> fetchHtmlWithRetry({
  required Future<String> Function() fetch,
  Duration delay = const Duration(milliseconds: 800),
  int minLength = 2000,
  int maxAttempts = 2,
}) async {
  HtmlFetchAttempt attempt = const HtmlFetchAttempt(html: '', tries: 0);
  for (int i = 0; i < maxAttempts; i++) {
    final String html = await fetch();
    attempt = HtmlFetchAttempt(html: html, tries: i + 1);
    if (!attempt.looksEmpty(minLength: minLength)) {
      return attempt;
    }
    if (i + 1 < maxAttempts) {
      await Future<void>.delayed(delay);
    }
  }
  return attempt;
}

/// 一次 HTML 抓取的结果（含诊断信息，**不记录原始 HTML**）。
class HtmlFetchAttempt {
  const HtmlFetchAttempt({required this.html, required this.tries});

  final String html;

  /// 第几次尝试（1 = 首次）。
  final int tries;

  int get length => html.length;

  /// 是否含课表关键字（既用于诊断，也用于判断是否需要重试）。
  bool get hasTimetableMark =>
      RegExp(r'timetable|课程|节次|课表|周次', caseSensitive: false).hasMatch(html);

  /// "空壳"：太短或不含课表关键字。
  bool looksEmpty({int minLength = 2000}) => length < minLength || !hasTimetableMark;

  /// 诊断行：`html 长度=N / 是否含课表关键字=true|false（第 N 次）`
  String get diagnostics =>
      'html 长度=$length / 是否含课表关键字=$hasTimetableMark（第 $tries 次）';
}

/// 【t151】`PARSE_EMPTY`（解析出 0 门课）也要可重试：最多 3 次（首发 + 2 次重试），间隔 800ms。
///
/// 背景：页面可能处于"课表框架已渲染、课程行还没填"的中间态 —— HTML 够长、含课表字样，
/// 但 Rust 解析出 0 门课。用户手动重试就好，说明只是**时机**问题。
///
/// 每次尝试都记录：`html 长度 / 课程行数 / 是否 PARSE_EMPTY`，失败消息一并带给用户。
Future<ParseRetryResult> importHtmlWithParseRetry({
  required Future<String> Function() fetchHtml,
  required Future<ImportOutcome> Function(String html) importHtml,
  int maxAttempts = 3,
  Duration delay = const Duration(milliseconds: 800),
  int Function(String html)? rowCountOf,
}) async {
  final List<ParseAttempt> attempts = <ParseAttempt>[];
  String html = '';
  ImportOutcome? lastOutcome;
  for (int i = 1; i <= maxAttempts; i++) {
    try {
      html = await fetchHtml();
    } on Object {
      // 取不到就用上一次的 html 再解析一次（比直接失败更有用）
    }
    lastOutcome = await importHtml(html);
    attempts.add(ParseAttempt(
      index: i,
      htmlLength: html.length,
      rowCount: rowCountOf?.call(html) ?? -1,
      parseEmpty: lastOutcome.imported == 0,
    ));
    if (lastOutcome.imported > 0) {
      return ParseRetryResult(outcome: lastOutcome, attempts: attempts);
    }
    if (i < maxAttempts) {
      await Future<void>.delayed(delay);
    }
  }
  return ParseRetryResult(outcome: lastOutcome!, attempts: attempts);
}

/// 一次解析尝试的记录（诊断用，不含 HTML 原文）。
class ParseAttempt {
  const ParseAttempt({
    required this.index,
    required this.htmlLength,
    required this.rowCount,
    required this.parseEmpty,
  });

  final int index;
  final int htmlLength;
  final int rowCount;
  final bool parseEmpty;

  String get diagnostics =>
      '第 $index 次：html 长度=$htmlLength / 课程行数=${rowCount < 0 ? '未知' : rowCount} / '
      'PARSE_EMPTY=${parseEmpty ? '是' : '否'}';
}

/// 带重试的导入结果。
class ParseRetryResult {
  const ParseRetryResult({required this.outcome, required this.attempts});

  final ImportOutcome outcome;
  final List<ParseAttempt> attempts;

  bool get ok => outcome.imported > 0;

  String get diagnostics {
    final String detail = attempts.map((ParseAttempt a) => a.diagnostics).join('；');
    final String cardId = describeCachedCardId();
    return '$detail；$cardId';
  }
}

/// 课表服务：HTML → Rust 核心（解析 + SQLite）→ 列表。
class TimetableService {
  TimetableService({
    CoreApi? core,
    String? dbPath,
    String? exportDir,
    bool Function(String path)? pathExists,
  })  : _core = core,
        _dbPath = dbPath,
        _exportDir = exportDir,
        _pathExists = pathExists;

  final CoreApi? _core;
  final String? _dbPath;
  final String? _exportDir;
  final bool Function(String path)? _pathExists;

  /// 数据库路径（Windows: %LOCALAPPDATA%\yunong\timetable.db）。
  ///
  /// 用环境变量而不是 path_provider：后者是插件，会把 Windows 构建绑到符号链接上。
  String get dbPath {
    final override = _dbPath;
    if (override != null) {
      return override;
    }
    final env = Platform.environment;
    final base = env['LOCALAPPDATA'] ??
        env['APPDATA'] ??
        env['HOME'] ??
        Directory.current.path;
    return <String>[base, 'yunong', 'timetable.db'].join(Platform.pathSeparator);
  }

  CoreApi get _library => _core ?? CoreLibrary.instance();

  /// 导入一份课表 HTML（WebView 抓取或调试粘贴都走这里）。
  Future<ImportOutcome> importHtml({
    required String html,
    required String term,
  }) async {
    final CoreEnvelope envelope;
    try {
      envelope = _library.importTimetable(
        html: html,
        term: term,
        dbPath: dbPath,
      );
    } on CoreUnavailable {
      rethrow;
    } on Object catch (err) {
      throw describeFailure('INTERNAL', '调用端侧核心失败：$err');
    }
    if (!envelope.ok) {
      throw describeFailure(envelope.code, envelope.message);
    }
    final data = envelope.data ?? const <String, dynamic>{};
    return ImportOutcome(
      term: data['term'] as String? ?? term,
      imported: data['imported'] as int? ?? 0,
      warnings: (data['warnings'] as List<dynamic>? ?? const <dynamic>[])
          .map((item) => item.toString())
          .toList(growable: false),
    );
  }

  /// 门户周数据 JSON 入库（W6 新形态）。
  ///
  /// 合并规则（相邻节次、周集合压缩）在 Rust 侧；这里只负责把 JSON 送下去并把
  /// `partial/missing_weeks` 如实带回 UI。
  Future<ScheduleImportOutcome> importScheduleJson({
    required String scheduleJson,
    required String term,
  }) async {
    final CoreEnvelope envelope;
    try {
      envelope = _library.importScheduleJson(
        json: scheduleJson,
        term: term,
        dbPath: dbPath,
      );
    } on CoreUnavailable {
      rethrow;
    } on Object catch (err) {
      throw describeFailure('INTERNAL', '调用端侧核心失败：$err');
    }
    if (!envelope.ok) {
      throw describeFailure(envelope.code, envelope.message);
    }
    final data = envelope.data ?? const <String, dynamic>{};
    return ScheduleImportOutcome(
      term: data['term'] as String? ?? term,
      imported: data['imported'] as int? ?? 0,
      warnings: (data['warnings'] as List<dynamic>? ?? const <dynamic>[])
          .map((dynamic item) => item.toString())
          .toList(growable: false),
      weeksSeen: (data['weeks_seen'] as List<dynamic>? ?? const <dynamic>[])
          .map((dynamic item) => item is int ? item : int.tryParse('$item') ?? 0)
          .where((int week) => week > 0)
          .toList(growable: false),
      missingWeeks: (data['missing_weeks'] as List<dynamic>? ?? const <dynamic>[])
          .map((dynamic item) => item is int ? item : int.tryParse('$item') ?? 0)
          .where((int week) => week > 0)
          .toList(growable: false),
      partial: data['partial'] == true,
      weekcount: data['weekcount'] as int?,
    );
  }

  /// 设置/修正学期开始日（`YYYY-MM-DD`，空串=清空）。
  Future<void> setTermStart({required String term, required String startsOn}) async {
    final envelope = _library.setTermStart(term: term, startsOn: startsOn, dbPath: dbPath);
    if (!envelope.ok) {
      throw describeFailure(envelope.code, envelope.message);
    }
  }

  /// 学期记录（含用户确认过的开学日）。
  Future<List<TermSummary>> listTermRecords() async {
    final envelope = _library.listTerms(dbPath: dbPath);
    if (!envelope.ok) {
      throw describeFailure(envelope.code, envelope.message);
    }
    final terms = (envelope.data?['terms'] as List<dynamic>? ?? const <dynamic>[]);
    return terms.whereType<Map<String, dynamic>>().map((Map<String, dynamic> item) {
      return TermSummary(
        term: item['term'] as String? ?? '',
        startsOn: item['starts_on'] as String? ?? '',
      );
    }).where((TermSummary item) => item.term.isNotEmpty).toList(growable: false);
  }

  /// ICS 导出目录。
  ///
  /// Windows 优先 `%USERPROFILE%\Downloads`：用户能在资源管理器里直接找到、双击导入日历；
  /// 没有该目录时退回应用数据目录 `<base>\yunong\exports`（不往用户目录乱写）。
  /// 其它平台用应用数据目录（沙箱内，避免申请存储权限）。
  String get exportDirectory {
    final override = _exportDir;
    if (override != null) {
      return override;
    }
    final env = Platform.environment;
    if (Platform.isWindows) {
      final profile = env['USERPROFILE'] ?? '';
      final downloads = <String>[profile, 'Downloads'].join(Platform.pathSeparator);
      if (profile.isNotEmpty && Directory(downloads).existsSync()) {
        return downloads;
      }
    }
    final base = env['LOCALAPPDATA'] ?? env['APPDATA'] ?? env['HOME'] ?? Directory.current.path;
    return <String>[base, 'yunong', 'exports'].join(Platform.pathSeparator);
  }

  /// 导出某学期课表为 `.ics`（W7）。
  ///
  /// 学期开始日未确认时后端返回 `TERM_START_UNKNOWN`，这里原样抛给 UI（有专门文案）。
  Future<IcsExportResult> exportIcs({required String term}) async {
    final directory = exportDirectory;
    final path = uniqueIcsPath(
      directory: directory,
      fileName: icsFileName(term),
      exists: _pathExists ?? (String candidate) => File(candidate).existsSync(),
    );
    final CoreEnvelope envelope;
    try {
      envelope = _library.exportIcs(term: term, outPath: path, dbPath: dbPath);
    } on CoreUnavailable {
      rethrow;
    } on Object catch (err) {
      throw describeFailure('INTERNAL', '调用端侧核心失败：$err');
    }
    if (!envelope.ok) {
      throw describeFailure(envelope.code, envelope.message);
    }
    final data = envelope.data ?? const <String, dynamic>{};
    return IcsExportResult(
      path: data['path'] as String? ?? path,
      events: data['events'] as int? ?? 0,
    );
  }
  /// 查某学期的课程。
  Future<List<StoredCourse>> listCourses({required String term}) async {
    final envelope = _library.listCourses(term: term, dbPath: dbPath);
    if (!envelope.ok) {
      throw describeFailure(envelope.code, envelope.message);
    }
    final courses =
        (envelope.data?['courses'] as List<dynamic>? ?? const <dynamic>[]);
    return courses
        .whereType<Map<String, dynamic>>()
        .map(StoredCourse.fromJson)
        .toList(growable: false);
  }

  /// 所有学期（最近的在前）。
  Future<List<String>> listTerms() async {
    final envelope = _library.listTerms(dbPath: dbPath);
    if (!envelope.ok) {
      throw describeFailure(envelope.code, envelope.message);
    }
    final terms = (envelope.data?['terms'] as List<dynamic>? ?? const <dynamic>[]);
    return terms
        .map((item) => item is Map<String, dynamic> ? item['term'] : item)
        .map((item) => item?.toString() ?? '')
        .where((item) => item.isNotEmpty)
        .toList(growable: false);
  }
}

/// 内置调试样本：正常课表页 / 改版后的页面。
///
/// 定位是**确定性回归入口**（不需要 WebView 也能验通 解析→落库→查询），
/// 也是"解析失败不能静默"这条规则的现场演示。
class DebugFixtures {
  static const String samplePage = 'assets/fixtures/timetable_sample.html';
  static const String revampedPage = 'assets/fixtures/timetable_revamp.html';

  static Future<String> load(String asset) => rootBundle.loadString(asset);

  /// 课表页的抓取脚本说明（WebView 注入用，见 `webview_fetcher.dart`）。
  ///
  /// 这里保留一份纯文本契约，方便没有 WebView 的平台也能对脚本做静态检查。
  static String get extractionScriptDescription =>
      jsonEncode(<String, String>{
        'detect': '页面出现课表 table 或已登录特征 → 回传 login_done',
        'extract': '取 table#timetable 的 outerHTML，postMessage 回传 timetable',
        'fallback': '找不到表 → 回传 error(HTML_CHANGED)，',
        'privacy': '不读取、不回传 Cookie；脚本不写日志',
      });
}
