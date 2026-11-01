// 课表页：空态 / 成功 / 解析失败三态 + 取数入口。
//
// 失败姿势（v0 的教训，docs/03 §8.2）：页面改版时必须**显式报错**，
// 既不能显示"这学期没课"，也不能把空结果覆盖掉旧课表。

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show Clipboard, ClipboardData;

import '../core/ffi_bindings.dart';
import 'portal_api.dart';
import 'dart:async';

import 'campus_entries.dart';
import '../core/timetable_service.dart';
import 'timetable/course_palette.dart';
import 'timetable/timetable_grid.dart';
import 'timetable/timetable_notices.dart';

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

  /// 最近一次 ICS 导出结果（成功时显示路径与事件数）。
  IcsExportResult? _exportResult;

  /// 最近一次动作（失败卡片的重试按钮复用它）。
  Future<void> Function()? _lastAction;

  /// 学期开始日：**按学期**取建议值（没有人工验证过的学期就是 `null`，必须由用户确认）。
  /// 确认后持久化到 `terms.starts_on`，下次直接读库。
  DateTime? _termStartDraft;
  bool _termStartSaved = false;

  /// 本学期总周数（来自接口 weekcount；未知时允许自由翻页）
  int? _weekcount;

  /// 当前查看的周（默认定位到"今天所在周"，算不出来才回第 1 周）
  int? _selectedWeek;
  String? _notice;

  /// 窄窗口下是否手动展开周末列（< 560 且本周周末无课时默认折叠）。
  bool _showWeekend = false;

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

  /// 操作条统一用 compact 密度：1280×720 下省出来的高度换成"整周可见"。
  static const ButtonStyle _compactButtonStyle = ButtonStyle(
    visualDensity: VisualDensity.compact,
    padding: WidgetStatePropertyAll<EdgeInsets>(
      EdgeInsets.symmetric(horizontal: 12),
    ),
    minimumSize: WidgetStatePropertyAll<Size>(Size(0, 36)),
  );

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: <Widget>[
        Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 16, 6),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: <Widget>[
              // 用 Wrap 而不是 Row：按钮文案较长，窄窗口（含 800×600 的 widget test 视口）
              // 下 Row 会直接溢出；Wrap 会自动折行。
              // 按钮统一 compact 密度：1280×720 下这一行只占 36px 高（默认 48px），
              // 省下来的高度直接换成"整周可见"。
              Wrap(
                spacing: 8,
                runSpacing: 6,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: <Widget>[
                  SizedBox(
                    width: 150,
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
                    style: _compactButtonStyle,
                    icon: const Icon(Icons.login, size: 18),
                    label: const Text('登录并抓取课表'),
                  ),
                  OutlinedButton.icon(
                    onPressed: _busy ? null : () => _importFixture(revamped: false),
                    style: _compactButtonStyle,
                    icon: const Icon(Icons.description_outlined, size: 18),
                    label: const Text('导入内置样本'),
                  ),
                  OutlinedButton.icon(
                    onPressed: _busy ? null : () => _importFixture(revamped: true),
                    style: _compactButtonStyle,
                    icon: const Icon(Icons.bug_report_outlined, size: 18),
                    label: const Text('模拟页面改版'),
                  ),
                  OutlinedButton.icon(
                    onPressed: _busy ? null : _exportIcs,
                    style: _compactButtonStyle,
                    icon: const Icon(Icons.event_available, size: 18),
                    label: const Text('导出到日历'),
                  ),
                  TextButton.icon(
                    onPressed: _busy ? null : _importFromClipboard,
                    style: _compactButtonStyle,
                    icon: const Icon(Icons.content_paste, size: 18),
                    label: const Text('从剪贴板导入'),
                  ),
                  // 「本学期 N 门课 / 最近一次抓取结果」合成一个 chip。
                  // 原来它单独占一行（"已从门户接口抓取，本学期 9 门课"），
                  // 1280×720 下白吃 22px 竖向空间，导致 10 节看不全。
                  if (_courses.isNotEmpty || _notice != null)
                    ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: 420),
                      child: Container(
                        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
                        decoration: BoxDecoration(
                          color: theme.colorScheme.surfaceContainerHighest.withValues(alpha: 0.55),
                          borderRadius: BorderRadius.circular(99),
                        ),
                        child: Row(
                          mainAxisSize: MainAxisSize.min,
                          children: <Widget>[
                            Icon(
                              Icons.info_outline,
                              size: 15,
                              color: theme.colorScheme.onSurfaceVariant,
                            ),
                            const SizedBox(width: 6),
                            Flexible(
                              child: Text(
                                _notice ?? '本学期 ${_courses.length} 门课',
                                maxLines: 1,
                                overflow: TextOverflow.ellipsis,
                                style: theme.textTheme.bodySmall,
                              ),
                            ),
                          ],
                        ),
                      ),
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
        _buildExportCard(theme),
        if (_failure != null)
          _FailureCard(
            failure: _failure!,
            onRetry: _retryFailedAction,
            onConfirmTermStart: _failure!.code == 'TERM_START_UNKNOWN'
                ? () => unawaited(_goToTermStart())
                : null,
          ),
        Expanded(child: _buildBody(theme)),
      ],
    );
  }

  /// 今天所在周（用学期锚点算）；算不出来返回 null。
  int? get _todayWeek {
    final draft = _termStartDraft;
    if (draft == null) {
      return null;
    }
    final week = currentWeek(termStart: draft, today: DateTime.now());
    if (week == null || week < 1) {
      return null;
    }
    return week;
  }

  /// 视图默认周：当前周优先；算不出来 / 超出总周数时回第 1 周。
  int _resolveInitialWeek() {
    final week = _todayWeek;
    if (week == null) {
      return 1;
    }
    final total = _weekcount;
    if (total != null && week > total) {
      return 1;
    }
    return week;
  }

  int get _week => _selectedWeek ??= _resolveInitialWeek();

  void _changeWeek(int delta) {
    final next = _week + delta;
    if (next < 1) {
      return;
    }
    final total = _weekcount;
    if (total != null && next > total) {
      return;
    }
    setState(() => _selectedWeek = next);
  }

  /// 当前周要展示的课（按 courses.weeks 过滤）。
  List<StoredCourse> get _visibleCourses {
    final week = _week;
    return _courses
        .where((StoredCourse course) => occursInWeek(course.weeks, week))
        .toList(growable: false);
  }

  bool get _isThisWeek => _todayWeek != null && _week == _todayWeek;

  /// `第 N 周（MM-DD ~ MM-DD）`。是不是本周由标题旁的 pill 表达，不重复写在文案里。
  String get _weekTitle {
    final week = _week;
    final draft = _termStartDraft;
    final range = draft == null ? '' : weekRangeLabel(termStart: draft, week: week);
    return range.isEmpty ? '第 $week 周' : '第 $week 周（$range）';
  }

  /// 整学期的「课程名 → 色板槽位」（切周不换色，见 `course_palette.dart`）。
  Map<String, int> get _courseTints =>
      assignCourseTints(_courses.map((StoredCourse course) => course.name));

  /// 本周 7 天的日期；学期开始日未确认时返回 `null`——**不猜日期**。
  List<DateTime>? get _weekDates {
    final draft = _termStartDraft;
    if (draft == null) {
      return null;
    }
    final start = draft.add(Duration(days: 7 * (_week - 1)));
    return List<DateTime>.generate(7, (int index) => start.add(Duration(days: index)));
  }

  /// 今天所在列（1=周一 … 7=周日）：只有"查看的是本周 + 已确认学期开始日"才给。
  int? get _todayColumn => _isThisWeek ? DateTime.now().weekday : null;

  /// 今天第一节的节次：进入/切到本周时滚到那里。今天没课就不滚（返回 null）。
  int? get _todayFirstPeriod {
    final column = _todayColumn;
    if (column == null) {
      return null;
    }
    final List<int> starts = _visibleCourses
        .where((StoredCourse course) => course.weekday == column)
        .map((StoredCourse course) => course.startPeriod)
        .toList(growable: false);
    if (starts.isEmpty) {
      return null;
    }
    return starts.reduce((int a, int b) => a < b ? a : b);
  }

  /// 相邻"确实有课"的周号（各取最近一个），给「本周无课」当跳板。
  List<int> get _nearestCourseWeeks {
    final int current = _week;
    final Set<int> weeksWithCourses = <int>{};
    for (final StoredCourse course in _courses) {
      weeksWithCourses.addAll(expandWeeks(course.weeks));
    }
    final List<int> before =
        weeksWithCourses.where((int week) => week < current).toList()..sort();
    final List<int> after =
        weeksWithCourses.where((int week) => week > current).toList()..sort();
    return <int>[
      if (before.isNotEmpty) before.last,
      if (after.isNotEmpty) after.first,
    ];
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
      return _buildNoData(theme);
    }
    final visible = _visibleCourses;
    return LayoutBuilder(
      builder: (BuildContext context, BoxConstraints constraints) {
        final bool narrow = constraints.maxWidth < kNarrowWidth;
        final bool weekendHasCourses =
            visible.any((StoredCourse course) => course.weekday >= 6);
        final bool folded = narrow && !_showWeekend && !weekendHasCourses;
        return Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: <Widget>[
            _buildWeekBar(
              theme,
              canToggleWeekend: narrow && !weekendHasCourses,
              folded: folded,
            ),
            Expanded(
              child: visible.isEmpty
                  ? WeekEmptyCard(
                      week: _week,
                      totalCourses: _courses.length,
                      weekCount: _weekcount,
                      nearestWeeks: _nearestCourseWeeks,
                      onSelectWeek: (int week) => setState(() => _selectedWeek = week),
                      canPrevious: _week > 1,
                      canNext: _week < (_weekcount ?? 1 << 30),
                      onPreviousWeek: () => _changeWeek(-1),
                      onNextWeek: () => _changeWeek(1),
                    )
                  : Padding(
                      padding: const EdgeInsets.fromLTRB(12, 6, 12, 12),
                      child: TimetableGrid(
                        courses: visible,
                        week: _week,
                        dayCount: folded ? 5 : 7,
                        assignment: _courseTints,
                        dates: _weekDates,
                        todayIndex: _todayColumn,
                        scrollToPeriod: _todayFirstPeriod,
                      ),
                    ),
            ),
          ],
        );
      },
    );
  }

  /// 「从未导入」的空态（中性；契约要求解析失败时**不能**显示这一屏）。
  Widget _buildNoData(ThemeData theme) {
    return Center(
      child: SingleChildScrollView(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            Icon(
              Icons.event_note_outlined,
              size: 44,
              color: theme.colorScheme.onSurfaceVariant,
            ),
            const SizedBox(height: 12),
            Text('还没有导入课表', style: theme.textTheme.titleMedium),
            const SizedBox(height: 8),
            Text(
              '在校务系统登录一次，或在调试环境导入一份课表 HTML。\n'
              '学期：$_currentTerm',
              textAlign: TextAlign.center,
              style: theme.textTheme.bodySmall
                  ?.copyWith(color: theme.colorScheme.onSurfaceVariant),
            ),
          ],
        ),
      ),
    );
  }

  /// 周次栏：`‹ 标题 ›` + 「本周」pill；附加动作（显示周末 / 回到本周）单独一行，
  /// **不和标题抢宽度**——窄窗口下这是最容易溢出的地方。
  Widget _buildWeekBar(
    ThemeData theme, {
    required bool canToggleWeekend,
    required bool folded,
  }) {
    final bool canBack = _todayWeek != null && !_isThisWeek;
    final List<Widget> extras = <Widget>[
      if (canToggleWeekend)
        TextButton(
          onPressed: () => setState(() => _showWeekend = !_showWeekend),
          child: Text(folded ? '显示周末' : '收起周末'),
        ),
      if (canBack)
        TextButton(
          onPressed: () => setState(() => _selectedWeek = _todayWeek),
          child: const Text('回到本周'),
        ),
    ];
    return Padding(
      padding: const EdgeInsets.fromLTRB(8, 4, 8, 0),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          Row(
            children: <Widget>[
              IconButton(
                tooltip: '上一周',
                visualDensity: VisualDensity.compact,
                onPressed: _week > 1 ? () => _changeWeek(-1) : null,
                icon: const Icon(Icons.chevron_left),
              ),
              Expanded(
                child: Row(
                  mainAxisAlignment: MainAxisAlignment.center,
                  children: <Widget>[
                    Flexible(
                      child: Text(
                        _weekTitle,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        textAlign: TextAlign.center,
                        style: theme.textTheme.titleMedium
                            ?.copyWith(fontWeight: FontWeight.w600),
                      ),
                    ),
                    if (_isThisWeek) ...<Widget>[
                      const SizedBox(width: 8),
                      Container(
                        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
                        decoration: BoxDecoration(
                          color: theme.colorScheme.primaryContainer,
                          borderRadius: BorderRadius.circular(99),
                        ),
                        child: Text(
                          '本周',
                          style: theme.textTheme.labelSmall?.copyWith(
                            color: theme.colorScheme.onPrimaryContainer,
                            fontWeight: FontWeight.w700,
                          ),
                        ),
                      ),
                    ],
                  ],
                ),
              ),
              IconButton(
                tooltip: '下一周',
                visualDensity: VisualDensity.compact,
                onPressed: _week < (_weekcount ?? 1 << 30) ? () => _changeWeek(1) : null,
                icon: const Icon(Icons.chevron_right),
              ),
            ],
          ),
          if (extras.isNotEmpty)
            Wrap(
              alignment: WrapAlignment.end,
              spacing: 8,
              children: extras,
            ),
        ],
      ),
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
        _weekcount = outcome.weekcount;
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
      // 用琥珀色而不是红色：这是"等你确认"，不是"出错了"——
      // 红色只留给解析失败（见 `_FailureCard`），两者不能长得一样。
      return TermStartPrompt(
        title: '还没确认这学期的开始日',
        subtitle: '确认后才能标出「今天」和当前周；课表内容仍可查看。（$_currentTerm）',
        onPickDate: _editTermStart,
      );
    }
    if (!_termStartSaved) {
      // 有校历建议值、但用户还没点过确认：同一处琥珀横幅，多给一个一键确认。
      return TermStartPrompt(
        title: '学期开始日还没确认（校历建议 ${_shortDate(draft)}）',
        subtitle: '确认后按这天换算每一周的日期；课表内容与周次不受影响。',
        suggestedLabel: '确认 ${_shortDate(draft)}',
        onUseSuggested: () => unawaited(_saveTermStart()),
        onPickDate: _editTermStart,
      );
    }
    final week = _currentWeek;
    final weekOne = weekRangeLabel(termStart: draft, week: 1);
    // 已确认：压成**一行**（约 36px）。
    // 之前是两行卡片（74px），1280×720 下正好把第 10 节挤出屏幕；
    // 信息层级上它也只是"参考值"，不该占两行。
    final String detail = <String>[
      '学期开始日 ${formatIsoDate(draft)}',
      '第 1 周 $weekOne',
      if (week != null && week > 0) '今天第 $week 周',
    ].join(' · ');
    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 2, 8, 0),
      child: Row(
        children: <Widget>[
          Icon(
            Icons.event_available_outlined,
            size: 16,
            color: theme.colorScheme.onSurfaceVariant,
          ),
          const SizedBox(width: 6),
          Expanded(
            child: Text(
              detail,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.bodySmall,
            ),
          ),
          TextButton(
            onPressed: _editTermStart,
            style: TextButton.styleFrom(
              visualDensity: VisualDensity.compact,
              minimumSize: const Size(0, 32),
              padding: const EdgeInsets.symmetric(horizontal: 10),
            ),
            child: const Text('修改'),
          ),
        ],
      ),
    );
  }

  static String _shortDate(DateTime date) =>
      '${date.month.toString().padLeft(2, '0')}-${date.day.toString().padLeft(2, '0')}';
  /// 「导出到日历」：成功显示路径 + 事件数；TERM_START_UNKNOWN 引导到学期开始日卡片。
  Future<void> _exportIcs() async {
    _lastAction = _exportIcs;
    setState(() {
      _busy = true;
      _failure = null;
      _exportResult = null;
    });
    try {
      final result = await widget.service.exportIcs(term: _currentTerm);
      if (!mounted) {
        return;
      }
      setState(() {
        _exportResult = result;
        _notice = '已导出 ${result.events} 个事件';
      });
    } on TimetableException catch (failure) {
      if (!mounted) {
        return;
      }
      setState(() => _failure = failure);
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

  /// TERM_START_UNKNOWN：直接打开学期开始日确认（日期选择器），确认后再导出。
  Future<void> _goToTermStart() async {
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(content: Text('先确认学期开始日，再点「导出到日历」')),
    );
    await _editTermStart();
  }

  Future<void> _copyExportPath(String path) async {
    await Clipboard.setData(ClipboardData(text: path));
    if (!mounted) {
      return;
    }
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(content: Text('已复制导出路径')),
    );
  }

  /// 导出成功后的结果卡片：路径可复制 + 事件数。
  Widget _buildExportCard(ThemeData theme) {
    final result = _exportResult;
    if (result == null) {
      return const SizedBox.shrink();
    }
    return Container(
      margin: const EdgeInsets.fromLTRB(16, 4, 16, 4),
      padding: const EdgeInsets.all(10),
      decoration: BoxDecoration(
        color: theme.colorScheme.tertiaryContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: <Widget>[
          const Icon(Icons.event_available, size: 18),
          const SizedBox(width: 8),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: <Widget>[
                Text('已导出 ${result.events} 个事件到日历文件', style: theme.textTheme.bodyMedium),
                SelectableText(result.path, style: theme.textTheme.bodySmall),
              ],
            ),
          ),
          IconButton(
            tooltip: '复制路径',
            onPressed: () => _copyExportPath(result.path),
            icon: const Icon(Icons.copy, size: 18),
          ),
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
/// 只有"抓取/诊断"类错误才把原始信息显示给用户；导出/存储类只给可读文案
/// （需求：导出失败不要显示原始错误串）。
bool _shouldShowRawMessage(String code) => const <String>{
      'CAPTURE_TIMEOUT',
      'HTML_CHANGED',
      'PORTAL_PAGE',
      'AUTH_REQUIRED',
      'CARD_NOT_FOUND',
      'NETWORK',
      'CERT',
      'TIMEOUT',
      'LOGIN_TIMEOUT',
    }.contains(code);
class _FailureCard extends StatelessWidget {
  const _FailureCard({
    required this.failure,
    required this.onRetry,
    this.onConfirmTermStart,
  });

  final TimetableException failure;
  final Future<void> Function() onRetry;
  final void Function()? onConfirmTermStart;

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
          if (onConfirmTermStart != null)
            Align(
              alignment: Alignment.centerRight,
              child: FilledButton.icon(
                onPressed: onConfirmTermStart,
                icon: const Icon(Icons.edit_calendar, size: 18),
                label: const Text('去确认学期开始日'),
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
          // 错误码始终显示（便于反馈）；原始错误串只对"抓取/诊断"类错误展示，
          // 导出/存储类只给可读文案（需求：不要显示原始错误串）。
          SelectableText(
            _shouldShowRawMessage(failure.code) && failure.message.trim().isNotEmpty
                ? '错误码 ${failure.code} · ${failure.message}'
                : '错误码 ${failure.code}',
            style: theme.textTheme.bodySmall?.copyWith(
              color: theme.colorScheme.onErrorContainer.withValues(alpha: 0.75),
            ),
          ),
        ],
      ),
    );
  }
}
