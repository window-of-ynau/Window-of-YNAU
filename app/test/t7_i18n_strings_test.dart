// #69：i18n 约束测试 —— `lib/src/**` 不得再出现裸中文字面量（注释与 l10n 字符串文件除外）。
//
// 现状是**增量迁移**：已迁移 UI 文件（chat_top_bar / campus_entries / provider_card /
// chat_page 的 composer hint）走 `lib/src/l10n/strings.dart`；其余文件在 [_pendingMigration]
// 里登记为待迁移债务（只许缩小、不许新增）。一旦某个文件迁移完毕，就要从这份名单里删掉，
// 否则本测试会因「名单里有文件已经干净」而失败，防止债务清单腐烂。
//
// 扫描口径：
// - 只看普通单/双引号字符串（含 `r'...'`）；**忽略注释**（// 与 /* */）；
// - **忽略三引号多行字符串** —— 目前只用于注入 WebView 的 JS 脚本（如 portal_api 的
//   `kPortalScheduleScript`），不是 Flutter UI 文案；UI 文案不允许用三引号绕过；
// - 忽略 `lib/src/l10n/**`（字符串单一来源）。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/l10n/strings.dart';

/// 尚未迁移的文件 → 当前裸中文字面量数量（#69 渐进债务）。
///
/// 迁移完一个删一条；文件里**新增**一条裸中文也会让测试失败（数量必须 ≤ 登记值），
/// 这样旧债务不会继续变大。
const Map<String, int> _pendingMigration = <String, int>{
  'lib/src/campus/campus_bridge.dart': 40,
  'lib/src/campus/capture_log.dart': 57,
  'lib/src/campus/capture_readiness.dart': 11,
  'lib/src/campus/portal_api.dart': 7,
  'lib/src/campus/timetable/course_detail_sheet.dart': 11,
  'lib/src/campus/timetable/timetable_grid.dart': 12,
  'lib/src/campus/timetable/timetable_notices.dart': 8,
  'lib/src/campus/timetable_page.dart': 71,
  'lib/src/campus/webview_fetcher.dart': 22,
  'lib/src/campus/webview_session.dart': 7,
  'lib/src/chat/chat_page.dart': 49,
  'lib/src/chat/chat_settings_section.dart': 25,
  'lib/src/chat/model_selector.dart': 14,
  'lib/src/chat/tool_call_card.dart': 14,
  'lib/src/core/app_paths.dart': 9,
  'lib/src/core/auth_service.dart': 12,
  'lib/src/core/chat_service.dart': 19,
  'lib/src/core/ffi_bindings.dart': 17,
  'lib/src/core/local_search_service.dart': 7,
  'lib/src/core/memory_service.dart': 8,
  'lib/src/core/search_service.dart': 7,
  'lib/src/core/secret_service.dart': 7,
  'lib/src/core/timetable_service.dart': 33,
  'lib/src/memory/memory_page.dart': 22,
  'lib/src/settings/key_account_group.dart': 14,
  'lib/src/settings/settings_page.dart': 37,
  'lib/src/theme/theme_controller.dart': 8,
  'lib/src/theme/yunong_tokens.dart': 5,
};

final RegExp _cjk = RegExp(r'[\u4e00-\u9fff]');

/// 返回文件里所有「用户可见候选」中文字符串（跳过注释与三引号脚本）。
List<String> chineseStringLiterals(String source) {
  final List<String> found = <String>[];
  int i = 0;
  final int n = source.length;
  while (i < n) {
    final String c = source[i];
    // 行注释
    if (c == '/' && i + 1 < n && source[i + 1] == '/') {
      final int j = source.indexOf('\n', i);
      i = j < 0 ? n : j + 1;
      continue;
    }
    // 块注释
    if (c == '/' && i + 1 < n && source[i + 1] == '*') {
      final int j = source.indexOf('*/', i + 2);
      i = j < 0 ? n : j + 2;
      continue;
    }
    bool raw = false;
    if (c == 'r' && i + 1 < n && (source[i + 1] == "'" || source[i + 1] == '"')) {
      raw = true;
      i += 1;
    }
    final String q = source[i];
    if (q == "'" || q == '"') {
      final bool triple = source.startsWith(q * 3, i);
      int j = i + (triple ? 3 : 1);
      while (j < n) {
        if (!raw && source[j] == '\\') {
          j += 2;
          continue;
        }
        if (triple) {
          if (source.startsWith(q * 3, j)) {
            j += 3;
            break;
          }
        } else {
          if (source[j] == q) {
            j += 1;
            break;
          }
        }
        j += 1;
      }
      final String token = source.substring(i, j);
      if (!triple && _cjk.hasMatch(token)) {
        found.add(token);
      }
      i = j;
      continue;
    }
    i += 1;
  }
  return found;
}

String _normalize(String path) => path.replaceAll('\\', '/');

void main() {
  test('lib/src/** 不得新增裸中文字面量（注释 / l10n / 注入脚本除外）', () {
    final Directory root = Directory('lib/src');
    expect(root.existsSync(), isTrue, reason: '测试须在 app/ 目录下运行');

    final Map<String, int> offenders = <String, int>{};
    for (final FileSystemEntity entity in root.listSync(recursive: true)) {
      if (entity is! File || !entity.path.endsWith('.dart')) {
        continue;
      }
      final String path = _normalize(entity.path);
      if (path.contains('lib/src/l10n/')) {
        continue;
      }
      final int count = chineseStringLiterals(entity.readAsStringSync()).length;
      if (count > 0) {
        offenders[path] = count;
      }
    }

    final List<String> unexpected = offenders.keys
        .where((String path) => !_pendingMigration.containsKey(path))
        .toList()
      ..sort();
    // ignore: avoid_print
    print('i18n: ${offenders.length} 个待迁移文件 / ${_pendingMigration.length} 条登记');
    expect(
      unexpected,
      isEmpty,
      reason: '这些文件出现了裸中文字面量，请搬到 lib/src/l10n/strings.dart：\n${unexpected.join('\n')}',
    );

    // 债务只许缩小：既有文件的字面量数量必须与登记值一致（新增会失败、迁移完要更新名单）。
    final List<String> countDrift = <String>[];
    for (final MapEntry<String, int> entry in _pendingMigration.entries) {
      final int? actual = offenders[entry.key];
      if (actual != entry.value) {
        countDrift.add('${entry.key}: 登记=${entry.value} 实际=${actual ?? 0}');
      }
    }
    expect(
      countDrift,
      isEmpty,
      reason: '裸中文字面量数量变了（新增不允许；已迁移则把该文件从 _pendingMigration 删掉）：\n'
          '${countDrift.join('\n')}',
    );
  });

  test('l10n 字符串组存在且文案未变（zh-CN）', () {
    expect(ChatS.inputHint, '输入消息…');
    expect(ChatS.statusTooltip, '状态与联网搜索');
    expect(CampusS.portal, '教务门户');
    expect(CampusS.timetable, '我的课表');
    expect(SettingsS.addProvider, '添加供应商');
    expect(SettingsS.alreadyAdded, '已添加');
    expect(
      SettingsS.providerSubtitle('https://api.example.com', hasKey: true, broken: false),
      'https://api.example.com · Key 已保存',
    );
    expect(SettingsS.deleteProviderTitle('X'), '删除供应商「X」？');
  });
}
