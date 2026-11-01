// 课表页：空态 / 成功 / 解析失败三态 + 取数入口。
//
// 失败姿势（v0 的教训，docs/03 §8.2）：页面改版时必须**显式报错**，
// 既不能显示"这学期没课"，也不能把空结果覆盖掉旧课表。

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show Clipboard;

import '../core/ffi_bindings.dart';
import 'portal_api.dart';
import 'dart:async';

import 'campus_entries.dart';
import '../core/timetable_service.dart';

/// 课表页。
class TimetablePage extends StatefulWidget {
  const TimetablePage({
    super.key,
    required this.service,
    this.fetcher,
    this.initialTerm = '2026-2027-1',
    this.fixtureLoader,
  });

  final TimetableService service;

  /// 内置样本的读取方式。默认从 asset 读；测试可注入固定字符串，
  /// 避免 widget test 里的 asset I/O 让 `_busy` 长期挂在 true（进度条一直转，
  /// `pumpAndSettle` 永远不 settle）。
  final Future<String> Function(String asset)? fixtureLoader;

  /// 校务取数入口。`null` = 当前构建没有 WebView（调试路径仍可用）。
  final SchoolFetcher? fetcher;

  final String initialTerm;

  @override
  State<TimetablePage> createState() => _TimetablePageState();
}

class _TimetablePageState extends State<TimetablePage> {
  late final TextEditingController _term =
      TextEditingController(text: widget.initialTerm);

  bool _busy = false;
  List<StoredCourse> _courses = const <StoredCourse>[];
  List<String> _warnings = const <String>[];
  TimetableException? _failure;

  /// 最近一次动作（失败卡片的重试按钮复用它）。
  Future<void> Function()? _lastAction;

  /// 学期开始日：**按学期**取建议值（没有人工验证过的学期就是 `null`，必须由用户确认）。
  /// 确认后持久化到 `terms.starts_on`，下次直接读库。
  DateTime? _termStartDraft;
  bool _termStartSaved = false;
  String? _notice;

  @override
  void initState() {
    super.initState();
    // 只对"人工验证过的学期"给建议值；其它学期留空，等用户确认。
    _termStartDraft = suggestedTermStart(_currentTerm);
    unawaited(_loadTermStart());
  }

  @override
  void dispose() {
    _term.dispose();
    super.dispose();
  }

  String get _currentTerm => _term.text.trim();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: <Widget>[
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 16, 8),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: <Widget>[
              // 用 Wrap 而不是 Row：按钮文案较长，窄窗口（含 800×600 的 widget test 视口）
              // 下 Row 会直接溢出；Wrap 会自动折行。
              Wrap(
                spacing: 8,
                runSpacing: 8,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: <Widget>[
                  SizedBox(
                    width: 180,
                    child: TextField(
                      controller: _term,
                      decoration: const InputDecoration(
                        labelText: '学期',
                        hintText: '2026-2027-1',
                        isDense: true,
                      ),
                    ),
                  ),
                  FilledButton.icon(
                    onPressed: _busy ? null : _fetchFromCampus,
                    icon: const Icon(Icons.login),
                    label: const Text('登录并抓取课表'),
                  ),
                  OutlinedButton.icon(
                    onPressed: _busy ? null : () => _importFixture(revamped: false),
                    icon: const Icon(Icons.description_outlined),
                    label: const Text('导入内置样本'),
                  ),
                  OutlinedButton.icon(
                    onPressed: _busy ? null : () => _importFixture(revamped: true),
                    icon: const Icon(Icons.bug_report_outlined),
                    label: const Text('模拟页面改版'),
                  ),
                  TextButton.icon(
                    onPressed: _busy ? null : _importFromClipboard,
                    icon: const Icon(Icons.content_paste),
                    label: const Text('从剪贴板导入'),
                  ),
                ],
              ),
              const SizedBox(height: 8),
              Text(
                '取数说明：课表 HTML 只交给端侧核心解析；Cookie 只留在本机 WebView，'
                '不上传、不打印。调试按钮是确定性回归入口，不需要登录。',
                style: theme.textTheme.bodySmall?.copyWith(color: theme.hintColor),
              ),
            ],
          ),
        ),
        if (_busy) const LinearProgressIndicator(),
        if (_notice != null)
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
            child: Text(_notice!, style: theme.textTheme.bodySmall),
          ),
        if (_warnings.isNotEmpty)
          Container(
            margin: const EdgeInsets.fromLTRB(16, 4, 16, 4),
            padding: const EdgeInsets.all(10),
            decoration: BoxDecoration(
              color: theme.colorScheme.tertiaryContainer,
              borderRadius: BorderRadius.circular(8),
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: <Widget>[
                Text('解析不完整（已入库的课程仍可用）：',
                    style: theme.textTheme.labelLarge),
                for (final warning in _warnings)
                  Text('· $warning', style: theme.textTheme.bodySmall),
              ],
            ),
          ),
        _buildTermStartCard(theme),
        if (_failure != null)
          _FailureCard(failure: _failure!, onRetry: _retryFailedAction),
        Expanded(child: _buildBody(theme)),
      ],
    );
  }

  Widget _buildBody(ThemeData theme) {
    if (_busy && _courses.isEmpty && _failure == null) {
      return const Center(child: Text('正在解析…'));
    }
    if (_failure != null && _courses.isEmpty) {
      // 失败时**不显示空态**，也不提示"没课"。
      // 但只有"本来就没数据"时才隐藏 body：若之前已导入过课表，列表必须继续显示——
      // 失败不等于课表消失（见 widget test「导入失败后不清空已有列表」）。
      return const SizedBox.shrink();
    }
    if (_courses.isEmpty) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: <Widget>[
              const Icon(Icons.event_note_outlined, size: 48),
              const SizedBox(height: 12),
              Text('还没有导入课表', style: theme.textTheme.titleMedium),
              const SizedBox(height: 8),
              Text(
                '在校务系统登录一次，或在调试环境导入一份课表 HTML。\n'
                '学期：$_currentTerm',
                textAlign: TextAlign.center,
                style: theme.textTheme.bodySmall,
              ),
            ],
          ),
        ),
      );
    }
    return ListView.separated(
      padding: const EdgeInsets.all(16),
      itemCount: _courses.length,
      separatorBuilder: (_, __) => const Divider(height: 1),
      itemBuilder: (BuildContext context, int index) {
        final course = _courses[index];
        return ListTile(
          leading: CircleAvatar(child: Text(course.weekdayLabel)),
          title: Text(course.name),
          subtitle: Text(<String>[
            course.periodLabel,
            course.weeks,
            if (course.room != null) course.room!,
            if (course.teacher != null) course.teacher!,
          ].join(' · ')),
        );
      },
    );
  }

  // ---------------------------------------------------------------------------
  // 取数
  // ---------------------------------------------------------------------------

  Future<void> _fetchFromCampus() async {
    final fetcher = widget.fetcher;
    if (fetcher == null || !fetcher.isAvailable) {
      final reason = fetcher?.unavailableReason ?? '当前构建未包含 WebView（调试构建）';
      setState(() {
        _failure = TimetableException(
          code: 'NO_WEBVIEW',
          userHint: '暂时无法在应用内登录校务系统',
          message: '$reason；可以先用"导入内置样本"验证链路。',
        );
      });
      return;
    }
    _lastAction = _fetchFromCampus;
    // 主路径：门户接口（queryAWeekSchedule）；接口变了/抓不到模板再退回 DOM 抓取
    try {
      await _run(() async {
        final result = await fetcher.fetchSchedule(
          entryUrl: CampusEntry.timetable.url,
          term: _currentTerm,
        );
        final outcome = await widget.service.importScheduleJson(
          scheduleJson: result.toImportJson(term: _currentTerm),
          term: _currentTerm,
        );
        // 学期开始日不做任何推算：默认值 + 用户确认，见卡片
        _notice = _apiNotice(outcome);
        return ImportOutcome(
          term: outcome.term,
          imported: outcome.imported,
          warnings: outcome.warnings,
        );
      }, notice: '已从门户接口抓取');
    } on UnsupportedError {
      await _fetchFromCampusHtml(fetcher);
    } on TimetableException catch (failure) {
      // 接口没抓到模板（大多是因为用户还没在门户里打开课表卡片）：提示 + 退回 DOM 路径
      if (failure.code == 'CARD_NOT_FOUND') {
        await _fetchFromCampusHtml(fetcher, hint: '接口没抓到模板，已退回页面解析：${failure.userHint}');
        return;
      }
      if (!mounted) {
        return;
      }
      setState(() => _failure = failure);
    } on FetchException catch (failure) {
      if (!mounted) {
        return;
      }
      setState(() {
        _failure = TimetableException(
          code: failure.code,
          userHint: failure.userHint,
          message: failure.message ?? '',
        );
      });
    }
  }

  String _apiNotice(ScheduleImportOutcome outcome) {
    final parts = <String>[
      '已抓取 ${outcome.weeksSeen.length} 周',
      if (outcome.partial) '缺 ${outcome.missingWeeks.length} 周（已入库其余周）',
      if (outcome.weekcount != null) '共 ${outcome.weekcount} 周',
    ];
    return parts.join('，');
  }

  /// 兜底路径：抓 `table#timetable` 的 HTML（接口改版/抓不到模板时用）。
  Future<void> _fetchFromCampusHtml(SchoolFetcher fetcher, {String? hint}) async {
    await _run(() async {
      final page = await fetcher.fetchTimetablePage(
        entryUrl: CampusEntry.timetable.url,
      );
      return widget.service.importHtml(html: page.html, term: _currentTerm);
    }, notice: hint ?? '已从校务系统页面抓取');
  }
  Future<void> _importFixture({required bool revamped}) async {
    _lastAction = () => _importFixture(revamped: revamped);
    final asset =
        revamped ? DebugFixtures.revampedPage : DebugFixtures.samplePage;
    await _run(() async {
      final html = await (widget.fixtureLoader?.call(asset) ??
          DebugFixtures.load(asset));
      return widget.service.importHtml(html: html, term: _currentTerm);
    }, notice: revamped ? '已导入「页面改版」样本' : '已导入内置课表样本');
  }

  Future<void> _importFromClipboard() async {
    _lastAction = _importFromClipboard;
    await _run(() async {
      final data = await Clipboard.getData('text/plain');
      final html = data?.text ?? '';
      return widget.service.importHtml(html: html, term: _currentTerm);
    }, notice: '已从剪贴板导入');
  }

  /// 读取用户上次确认过的开学日（没有就用"按当前周推算"的候选值）。
  Future<void> _loadTermStart() async {
    try {
      final records = await widget.service.listTermRecords();
      final match = records.where((TermSummary item) => item.term == _currentTerm);
      if (match.isNotEmpty && match.first.hasStart) {
        final parsed = parseTermStart(match.first.startsOn);
        if (parsed != null && mounted) {
          setState(() {
            _termStartDraft = parsed;
            _termStartSaved = true;
          });
        }
      }
    } on Object {
      // 读取失败不影响主流程：界面会显示"按当前周推算"的候选值让用户确认
    }
  }

  /// 今天是第几周（按"今天 + 学期开始日"自己算；**不用** `zs`：那只是回显请求的周号）。
  int? get _currentWeek {
    final draft = _termStartDraft;
    if (draft == null) {
      return null;
    }
    return currentWeek(termStart: draft, today: DateTime.now());
  }
  Future<void> _editTermStart() async {
    final initial = _termStartDraft ?? DateTime.now();
    final picked = await showDatePicker(
      context: context,
      initialDate: initial,
      firstDate: DateTime(initial.year - 1),
      lastDate: DateTime(initial.year + 2),
      helpText: '学期开始日（第 1 周周一）',
    );
    if (picked != null && mounted) {
      setState(() => _termStartDraft = mondayOf(picked));
    }
  }

  Future<void> _saveTermStart() async {
    final draft = _termStartDraft;
    if (draft == null) {
      return;
    }
    try {
      await widget.service.setTermStart(term: _currentTerm, startsOn: formatIsoDate(draft));
      if (!mounted) {
        return;
      }
      setState(() {
        _termStartSaved = true;
        _notice = '已保存学期开始日：${formatIsoDate(draft)}';
      });
    } on TimetableException catch (failure) {
      if (!mounted) {
        return;
      }
      setState(() => _failure = failure);
    }
  }

  /// 学期开始日卡片：**显式展示日期与周区间**并要求确认（错一周整张课表日期全错）。
  ///
  /// 值来自"该学期人工验证过的锚点 / 用户上次确认的结果"，绝不按 `zs` 推算：
  /// `zs` 只是回显请求的周号，不是系统当前周。
  Widget _buildTermStartCard(ThemeData theme) {
    final draft = _termStartDraft;
    if (draft == null) {
      // 该学期没有锚点：**不显示任何具体日期**（宁可不显示，也不能显示错的），
      // 只提示用户确认一次；确认前课表按"第 N 周 · 星期 X"展示。
      return Container(
        margin: const EdgeInsets.fromLTRB(16, 4, 16, 4),
        padding: const EdgeInsets.all(10),
        decoration: BoxDecoration(
          color: theme.colorScheme.errorContainer,
          borderRadius: BorderRadius.circular(8),
        ),
        child: Row(
          children: <Widget>[
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: <Widget>[
                  Text('本学期（$_currentTerm）还没确认学期开始日',
                      style: theme.textTheme.bodyMedium),
                  Text('确认前只显示「第 N 周」，不显示具体日期；确认一次即可长期记住。',
                      style: theme.textTheme.bodySmall),
                ],
              ),
            ),
            FilledButton(onPressed: _editTermStart, child: const Text('选择日期')),
          ],
        ),
      );
    }
    final week = _currentWeek;
    final weekOne = weekRangeLabel(termStart: draft, week: 1);
    final subtitle = <String>[
      '第 1 周：$weekOne',
      if (week != null && week > 0) '今天按此算第 $week 周',
      _termStartSaved ? '已确认，日历换算按这天起算' : '未确认（可改）',
    ].join(' · ');
    return Container(
      margin: const EdgeInsets.fromLTRB(16, 4, 16, 4),
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: theme.colorScheme.secondaryContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: <Widget>[
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: <Widget>[
                Text('学期开始日（第 1 周周一）：${formatIsoDate(draft)}',
                    style: theme.textTheme.bodyMedium),
                Text(subtitle, style: theme.textTheme.bodySmall),
              ],
            ),
          ),
          TextButton(onPressed: _editTermStart, child: const Text('修改')),
          if (!_termStartSaved)
            FilledButton(onPressed: _saveTermStart, child: const Text('确认')),
        ],
      ),
    );
  }
  /// 失败卡片上的"重试"：重放最近一次动作（登录超时 / 网络错误最常见）。
  Future<void> _retryFailedAction() async {
    final action = _lastAction ?? _fetchFromCampus;
    await action();
  }

  /// 统一执行一次导入：成功刷新列表，失败保留旧列表并显示可读提示。
  Future<void> _run(
    Future<ImportOutcome> Function() action, {
    required String notice,
  }) async {
    setState(() {
      _busy = true;
      _failure = null;
      _notice = null;
      _warnings = const <String>[];
    });
    try {
      final outcome = await action();
      final courses = await widget.service.listCourses(term: outcome.term);
      if (!mounted) {
        return;
      }
      setState(() {
        _courses = courses;
        _warnings = outcome.warnings;
        _notice = '$notice，本学期 ${outcome.imported} 门课';
      });
    } on TimetableException catch (failure) {
      if (!mounted) {
        return;
      }
      setState(() {
        // 不清空已有列表：失败不该让用户以为课表没了
        _failure = failure;
      });
    } on CoreUnavailable catch (err) {
      if (!mounted) {
        return;
      }
      setState(() {
        _failure = TimetableException(
          code: 'CORE_UNAVAILABLE',
          userHint: '端侧核心未就绪',
          message: '$err',
        );
      });
    } finally {
      if (mounted) {
        setState(() => _busy = false);
      }
    }
  }
}

/// 失败卡片：大字号给用户看的那句 + 小字原始信息 + 明确"这不是空课表"。
class _FailureCard extends StatelessWidget {
  const _FailureCard({required this.failure, required this.onRetry});

  final TimetableException failure;
  final Future<void> Function() onRetry;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Container(
      margin: const EdgeInsets.fromLTRB(16, 8, 16, 8),
      padding: const EdgeInsets.all(14),
      decoration: BoxDecoration(
        color: theme.colorScheme.errorContainer,
        borderRadius: BorderRadius.circular(10),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: <Widget>[
          Row(
            children: <Widget>[
              Icon(Icons.error_outline, color: theme.colorScheme.onErrorContainer),
              const SizedBox(width: 8),
              Expanded(
                child: Text(
                  failure.userHint,
                  style: theme.textTheme.titleMedium?.copyWith(
                    color: theme.colorScheme.onErrorContainer,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: 6),
          Text(
            '这不是"没有课"：解析失败时我们不会写入空课表，已有课表也不会被覆盖。',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onErrorContainer,
            ),
          ),
          if (failure.code != 'CORE_UNAVAILABLE' && failure.code != 'NO_WEBVIEW')
            Align(
              alignment: Alignment.centerRight,
              child: TextButton.icon(
                onPressed: () => onRetry(),
                icon: const Icon(Icons.refresh, size: 18),
                label: const Text('重试'),
              ),
            ),
          const SizedBox(height: 4),
          SelectableText(
            '错误码 ${failure.code} · ${failure.message}',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onErrorContainer.withValues(alpha: 0.75),
            ),
          ),
        ],
      ),
    );
  }
}
