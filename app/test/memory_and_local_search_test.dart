// 记忆管理页 + 本地搜索入口的测试（fake 后端，离线）。
//
// 覆盖任务验收：记忆 CRUD 各一条、空态、已到期过滤、本地搜索命中/无命中、
// 引用插入输入框、未就绪时的诚实提示。

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/chat/chat_page.dart';
import 'package:yunong/src/core/chat_service.dart';
import 'package:yunong/src/core/local_search_service.dart';
import 'package:yunong/src/core/memory_service.dart';
import 'package:yunong/src/memory/memory_page.dart';

/// 内存版记忆后端（行为对齐 Rust：id 自增、按 updated_at 排序、支持到期）。
class FakeMemoryApi implements MemoryApi {
  FakeMemoryApi({this.now = 1700000000});

  final int now;
  final List<Map<String, dynamic>> rows = <Map<String, dynamic>>[];
  int _nextId = 1;
  int _clock = 0;

  @override
  List<Map<String, dynamic>> list({String kind = '', int limit = 200, required String dbPath}) {
    return rows
        .where((Map<String, dynamic> row) => kind.isEmpty || row['kind'] == kind)
        .take(limit)
        .toList();
  }

  @override
  Map<String, dynamic> upsert({
    required String kind,
    required String title,
    required String content,
    required String source,
    required String idOrEmpty,
    required String dbPath,
  }) {
    if (content.trim().isEmpty) {
      return <String, dynamic>{'ok': false, 'code': 'BAD_INPUT', 'message': '内容不能为空'};
    }
    _clock += 1;
    final updatedAt = now + _clock;
    if (idOrEmpty.isNotEmpty) {
      final id = int.tryParse(idOrEmpty) ?? 0;
      final index = rows.indexWhere((Map<String, dynamic> row) => row['id'] == id);
      if (index < 0) {
        return <String, dynamic>{'ok': false, 'code': 'BAD_INPUT', 'message': 'id 不存在'};
      }
      rows[index] = <String, dynamic>{
        ...rows[index],
        'kind': kind,
        'title': title,
        'content': content,
        'updated_at': updatedAt,
      };
      return <String, dynamic>{'ok': true, 'data': <String, dynamic>{'id': id, 'evicted': 0}};
    }
    final id = _nextId++;
    rows.add(<String, dynamic>{
      'id': id,
      'kind': kind,
      'title': title,
      'content': content,
      'source': source,
      'updated_at': updatedAt,
      'expires_at': null,
    });
    return <String, dynamic>{'ok': true, 'data': <String, dynamic>{'id': id, 'evicted': 0}};
  }

  @override
  Map<String, dynamic> delete({required String id, required String dbPath}) {
    final before = rows.length;
    rows.removeWhere((Map<String, dynamic> row) => '${row['id']}' == id);
    return <String, dynamic>{'ok': true, 'data': <String, dynamic>{'deleted': rows.length != before}};
  }
}

/// 本地搜索假后端。
class FakeLocalSearchBackend implements LocalSearchBackend {
  FakeLocalSearchBackend(this.hits);

  final List<Map<String, dynamic>> hits;
  final List<String> queries = <String>[];

  @override
  List<Map<String, dynamic>> search({
    required String query,
    required int limit,
    required String dbPath,
  }) {
    queries.add(query);
    return hits.take(limit).toList();
  }
}

class _NoopChatBackend implements ChatBackend {
  @override
  int start(String requestJson, bool Function(String chunkJson) onChunk) => 1;

  @override
  void cancel(int handle) {}
}

Future<void> _pumpMemory(WidgetTester tester, MemoryService service) async {
  await tester.pumpWidget(MaterialApp(home: MemoryPage(service: service)));
  await tester.pumpAndSettle();
}

void main() {
  group('记忆 CRUD', () {
    testWidgets('空态：提示"只存在这台设备上"', (WidgetTester tester) async {
      final service = MemoryService(api: FakeMemoryApi(), dbPath: '/tmp/m.db');
      await _pumpMemory(tester, service);
      expect(find.text('还没有记忆'), findsOneWidget);
      expect(find.textContaining('只存在这台设备上'), findsOneWidget);
    });

    testWidgets('新增一条后出现在列表里', (WidgetTester tester) async {
      final api = FakeMemoryApi();
      final service = MemoryService(api: api, dbPath: '/tmp/m.db');
      await _pumpMemory(tester, service);

      await tester.tap(find.text('新增'));
      await tester.pumpAndSettle();
      await tester.enterText(find.widgetWithText(TextField, '内容'), '周三下午没课');
      await tester.tap(find.text('保存'));
      await tester.pumpAndSettle();

      expect(api.rows, hasLength(1));
      expect(find.textContaining('周三下午没课'), findsOneWidget);
    });

    testWidgets('编辑一条会更新内容（id 不变）', (WidgetTester tester) async {
      final api = FakeMemoryApi();
      final service = MemoryService(api: api, dbPath: '/tmp/m.db');
      await service.save(kind: MemoryKind.fact, title: '作息', content: '早上 8 点有课');
      await _pumpMemory(tester, service);

      await tester.tap(find.byIcon(Icons.edit_outlined));
      await tester.pumpAndSettle();
      await tester.enterText(find.widgetWithText(TextField, '内容'), '改成 9 点');
      await tester.tap(find.text('保存'));
      await tester.pumpAndSettle();

      expect(api.rows, hasLength(1), reason: '编辑不能变成新增');
      expect(api.rows.single['content'], '改成 9 点');
    });

    testWidgets('删除一条后回到空态', (WidgetTester tester) async {
      final api = FakeMemoryApi();
      final service = MemoryService(api: api, dbPath: '/tmp/m.db');
      await service.save(kind: MemoryKind.note, title: 't', content: '临时笔记');
      await _pumpMemory(tester, service);

      await tester.tap(find.byIcon(Icons.delete_outline));
      await tester.pumpAndSettle();
      await tester.tap(find.text('删除'));
      await tester.pumpAndSettle();

      expect(api.rows, isEmpty);
      expect(find.text('还没有记忆'), findsOneWidget);
    });

    testWidgets('已到期条目默认隐藏，关掉开关后出现', (WidgetTester tester) async {
      final api = FakeMemoryApi();
      api.rows.add(<String, dynamic>{
        'id': 9,
        'kind': 'note',
        'title': '过期笔记',
        'content': '旧内容',
        'updated_at': 1,
        'expires_at': 2, // 1970 年，早就到期
      });
      final service = MemoryService(api: api, dbPath: '/tmp/m.db');
      await _pumpMemory(tester, service);
      expect(find.textContaining('过期笔记'), findsNothing);

      await tester.tap(find.byType(Switch));
      await tester.pumpAndSettle();
      expect(find.textContaining('过期笔记'), findsOneWidget);
      expect(find.text('已到期'), findsOneWidget);
    });
  });

  group('本地搜索入口', () {
    testWidgets('命中后点结果把引用插入输入框（带来源标题）', (WidgetTester tester) async {
      final backend = FakeLocalSearchBackend(<Map<String, dynamic>>[
        <String, dynamic>{'id': 1, 'title': '选课笔记', 'snippet': '通识课在周三'},
      ]);
      final search = LocalSearchService(backend: backend, dbPath: '/tmp/m.db');
      await tester.pumpWidget(MaterialApp(
        home: Scaffold(
          body: ChatPage(
            service: ChatService(backend: _NoopChatBackend()),
            localSearchService: search,
            memoryService: MemoryService(api: FakeMemoryApi(), dbPath: '/tmp/m.db'),
          ),
        ),
      ));
      await tester.pumpAndSettle();

      await tester.tap(find.byIcon(Icons.manage_search));
      await tester.pumpAndSettle();
      expect(find.textContaining('本地资料（只查这台设备，不联网）'), findsOneWidget);

      await tester.enterText(find.widgetWithText(TextField, '搜索本机记忆/资料…'), '选课');
      await tester.tap(find.text('搜索'));
      await tester.pumpAndSettle();

      expect(backend.queries, <String>['选课']);
      await tester.tap(find.text('选课笔记'));
      await tester.pumpAndSettle();

      final field = tester.widget<TextField>(find.byType(TextField).first);
      expect(field.controller!.text, contains('【本地资料·选课笔记】'));
      expect(field.controller!.text, contains('通识课在周三'));
    });

    testWidgets('无命中 → 明确说"本机没有匹配的资料"', (WidgetTester tester) async {
      final search = LocalSearchService(backend: FakeLocalSearchBackend(<Map<String, dynamic>>[]));
      await tester.pumpWidget(MaterialApp(
        home: Scaffold(
          body: ChatPage(
            service: ChatService(backend: _NoopChatBackend()),
            localSearchService: search,
            memoryService: MemoryService(api: FakeMemoryApi()),
          ),
        ),
      ));
      await tester.pumpAndSettle();
      await tester.tap(find.byIcon(Icons.manage_search));
      await tester.pumpAndSettle();
      await tester.enterText(find.widgetWithText(TextField, '搜索本机记忆/资料…'), '不存在的词');
      await tester.tap(find.text('搜索'));
      await tester.pumpAndSettle();

      expect(find.text('本机没有匹配的资料'), findsOneWidget);
      // 不能把"无命中"说成联网结果
      expect(find.textContaining('不联网'), findsOneWidget);
    });

    testWidgets('引擎未就绪 → 诚实提示"还没就绪"，不谎称无命中', (WidgetTester tester) async {
      final search = LocalSearchService(backend: _UnavailableBackend());
      await tester.pumpWidget(MaterialApp(
        home: Scaffold(
          body: ChatPage(
            service: ChatService(backend: _NoopChatBackend()),
            localSearchService: search,
            memoryService: MemoryService(api: FakeMemoryApi()),
          ),
        ),
      ));
      await tester.pumpAndSettle();
      await tester.tap(find.byIcon(Icons.manage_search));
      await tester.pumpAndSettle();
      await tester.enterText(find.widgetWithText(TextField, '搜索本机记忆/资料…'), 'x');
      await tester.tap(find.text('搜索'));
      await tester.pumpAndSettle();

      expect(find.textContaining('本地搜索还没就绪'), findsOneWidget);
      expect(find.text('本机没有匹配的资料'), findsNothing);
    });
  });
}

/// 模拟 `yunong_search_local` 还没合入。
class _UnavailableBackend implements LocalSearchBackend {
  @override
  List<Map<String, dynamic>> search({
    required String query,
    required int limit,
    required String dbPath,
  }) =>
      throw const LocalSearchUnavailable('端侧核心还没有 yunong_search_local');
}
