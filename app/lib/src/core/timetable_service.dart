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

/// 课表服务：HTML → Rust 核心（解析 + SQLite）→ 列表。
class TimetableService {
  TimetableService({CoreApi? core, String? dbPath})
      : _core = core,
        _dbPath = dbPath;

  final CoreApi? _core;
  final String? _dbPath;

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
