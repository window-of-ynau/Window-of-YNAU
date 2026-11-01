// 课表页：空态 / 成功 / 解析失败三态 + 取数入口。
//
// 失败姿势（v0 的教训，docs/03 §8.2）：页面改版时必须**显式报错**，
// 既不能显示"这学期没课"，也不能把空结果覆盖掉旧课表。

import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show Clipboard;

import '../core/ffi_bindings.dart';
import '../core/timetable_service.dart';

/// 课表页。
class TimetablePage extends StatefulWidget {
  const TimetablePage({
    super.key,
    required this.service,
    this.fetcher,
    this.initialTerm = '2026-2027-1',
  });

  final TimetableService service;

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
  String? _notice;

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
              Row(
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
                  const SizedBox(width: 12),
                  FilledButton.icon(
                    onPressed: _busy ? null : _fetchFromCampus,
                    icon: const Icon(Icons.login),
                    label: const Text('登录并抓取课表'),
                  ),
                  const SizedBox(width: 8),
                  OutlinedButton.icon(
                    onPressed: _busy ? null : () => _importFixture(revamped: false),
                    icon: const Icon(Icons.description_outlined),
                    label: const Text('导入内置样本'),
                  ),
                  const SizedBox(width: 8),
                  OutlinedButton.icon(
                    onPressed: _busy ? null : () => _importFixture(revamped: true),
                    icon: const Icon(Icons.bug_report_outlined),
                    label: const Text('模拟页面改版'),
                  ),
                  const SizedBox(width: 8),
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
        if (_failure != null) _FailureCard(failure: _failure!),
        Expanded(child: _buildBody(theme)),
      ],
    );
  }

  Widget _buildBody(ThemeData theme) {
    if (_busy && _courses.isEmpty && _failure == null) {
      return const Center(child: Text('正在解析…'));
    }
    if (_failure != null) {
      // 失败时**不显示空态**，也不提示"没课"
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
    await _run(() async {
      final page = await fetcher.fetchTimetablePage(
        entryUrl: 'https://jwxt.ynau.edu.cn/DefaultStu',
      );
      return widget.service.importHtml(html: page.html, term: _currentTerm);
    }, notice: '已从校务系统抓取');
  }

  Future<void> _importFixture({required bool revamped}) async {
    final asset =
        revamped ? DebugFixtures.revampedPage : DebugFixtures.samplePage;
    await _run(() async {
      final html = await DebugFixtures.load(asset);
      return widget.service.importHtml(html: html, term: _currentTerm);
    }, notice: revamped ? '已导入「页面改版」样本' : '已导入内置课表样本');
  }

  Future<void> _importFromClipboard() async {
    await _run(() async {
      final data = await Clipboard.getData('text/plain');
      final html = data?.text ?? '';
      return widget.service.importHtml(html: html, term: _currentTerm);
    }, notice: '已从剪贴板导入');
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
  const _FailureCard({required this.failure});

  final TimetableException failure;

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
