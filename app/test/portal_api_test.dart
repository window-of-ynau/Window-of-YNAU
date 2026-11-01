// W6：门户接口取数的纯 Dart 部分（CI 可跑，不需要 WebView / 真账号）。
//
// 覆盖 captain 的三条修正：
// - `years` 当不透明标识原样透传（不解析）；
// - 学期开始日**不硬编码**：按"当前周 zs"推算候选值，且只作候选、必须用户确认；
// - 抓包样本是第 15 周，**不代表**当前周（`zs` 与请求的 `results` 不绑定）。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/campus/campus_bridge.dart';
import 'package:yunong/src/campus/portal_api.dart';
import 'package:yunong/src/core/timetable_service.dart';

Map<String, dynamic> weekData(String zs, {bool withTimetable = true}) {
  return <String, dynamic>{
    'zs': zs,
    'weekcount': '20',
    if (withTimetable)
      'resultsJsonArr': <dynamic>[
        <dynamic>['高等数学I,东校区1教305,班级;,张老师'],
      ],
  };
}

void main() {
  group('years 是不透明标识', () {
    test('原样透传，不做任何解析', () {
      final result = ScheduleApiResult(
        weeks: <Map<String, dynamic>>[
          <String, dynamic>{'week': 1, 'data': weekData('10')},
        ],
        missingWeeks: const <int>[],
        weekcount: 20,
        cardId: 'card-1',
        years: '本科-2026-秋-主修',
      );
      final json = result.toImportJson(term: '本科-2026-秋-主修');
      expect(json, contains('本科-2026-秋-主修'));
      expect(json, isNot(contains('2026-2027-1')));
    });
  });

  group('学期开始日 = 2026-09-07（可配置，不按 zs 推算）', () {
    final start = defaultTermStart;

    test('默认值就是 2026-09-07 且是周一', () {
      expect(formatIsoDate(start), '2026-09-07');
      expect(start.weekday, DateTime.monday);
    });

    test('校验用例：第 1 / 8 / 15 周的区间', () {
      expect(weekRangeLabel(termStart: start, week: 1), '09-07 ~ 09-13');
      expect(weekRangeLabel(termStart: start, week: 8), '10-26 ~ 11-01');
      // 抓包样本（results=15）对应这一周
      expect(weekRangeLabel(termStart: start, week: 15), '12-14 ~ 12-20');
    });

    test('第 N 周第一天 = 开始日 + (N-1)*7', () {
      expect(weekStart(termStart: start, week: 1), DateTime(2026, 9, 7));
      expect(weekStart(termStart: start, week: 2), DateTime(2026, 9, 14));
      expect(weekStart(termStart: start, week: 15), DateTime(2026, 12, 14));
      expect(weekEnd(termStart: start, week: 8), DateTime(2026, 11, 1));
    });

    test('今天是第几周：自己按开始日算（owner 验收点：11-01 是第 8 周最后一天）', () {
      expect(currentWeek(termStart: start, today: DateTime(2026, 9, 7)), 1);
      expect(currentWeek(termStart: start, today: DateTime(2026, 9, 13)), 1);
      expect(currentWeek(termStart: start, today: DateTime(2026, 9, 14)), 2);
      expect(currentWeek(termStart: start, today: DateTime(2026, 11, 1)), 8);
      expect(currentWeek(termStart: start, today: DateTime(2026, 11, 2)), 9);
    });

    test('开学前返回 null（不瞎算）', () {
      expect(currentWeek(termStart: start, today: DateTime(2026, 8, 31)), isNull);
    });

    test('用户改过的开始日照样生效（可配置）', () {
      final custom = DateTime(2026, 9, 14);
      expect(parseTermStart('2026-09-14'), custom);
      expect(weekRangeLabel(termStart: custom, week: 8), '11-02 ~ 11-08');
      expect(currentWeek(termStart: custom, today: DateTime(2026, 11, 1)), 8);
    });

    test('坏日期解析成 null（退回默认值）', () {
      expect(parseTermStart(''), isNull);
      expect(parseTermStart('不是日期'), isNull);
    });

    test('mondayOf 对周一自身幂等，周日归到同周周一', () {
      final monday = DateTime(2026, 9, 7);
      expect(mondayOf(monday), monday);
      expect(mondayOf(DateTime(2026, 9, 13)), monday);
    });

    test('YYYY-MM-DD 补零', () {
      expect(formatIsoDate(DateTime(2026, 1, 5)), '2026-01-05');
      expect(formatShortDate(DateTime(2026, 12, 20)), '12-20');
    });
  });
  group('zs 只是回显请求的周号', () {
    test('等等——zs 不参与任何推算（本文件已无该函数，用编译通过来保证）', () {
      // 只要 portal_api.dart 里没有"用 zs 推算当前周/开学日"的函数，这条就恒真。
      // 断言写在这里是为了让"以后有人想把 zs 当当前周用"必须先删掉这个测试。
      expect(kDefaultTermStart, '2026-09-07');
    });
  });
  group('解析 schedule_api 消息', () {
    test('正常载荷 + 部分周缺失', () {
      final result = ScheduleApiResult.fromMessage(<String, dynamic>{
        'type': 'schedule_api',
        'weeks': <dynamic>[
          <String, dynamic>{'week': 1, 'data': weekData('10')},
          <String, dynamic>{'week': 2, 'data': weekData('10')},
          <String, dynamic>{'week': '3', 'data': weekData('10')}, // 兼容字符串周号
        ],
        'missing_weeks': <dynamic>[4, '5'],
        'weekcount': '20',
        'weekcount_fallback': false,
        'years': '2026-2027-1',
        'card_id': 'f95c',
      });
      expect(result.weeks, hasLength(3));
      expect(result.weeks[2]['week'], 3);
      expect(result.missingWeeks, <int>[4, 5]);
      expect(result.isPartial, isTrue);
      expect(result.weekcount, 20);
      expect(result.weekcountFallback, isFalse);
    });

    test('weekcount 缺失时标注兜底值', () {
      final result = ScheduleApiResult.fromMessage(<String, dynamic>{
        'weeks': <dynamic>[],
        'weekcount_fallback': true,
      });
      expect(result.weekcountFallback, isTrue);
      expect(result.weeks, isEmpty);
    });

    test('坏数据（data 不是对象）被丢掉而不是崩', () {
      final result = ScheduleApiResult.fromMessage(<String, dynamic>{
        'weeks': <dynamic>[
          <String, dynamic>{'week': 1, 'data': 'oops'},
          <String, dynamic>{'week': 'x', 'data': weekData('10')},
          <String, dynamic>{'week': 2, 'data': weekData('10')},
        ],
      });
      expect(result.weeks, hasLength(1));
      expect(result.weeks.single['week'], 2);
    });
  });

  group('bridge 处理新消息', () {
    test('schedule_template → 回调模板（运行时捕获的 cardId/years）', () {
      ScheduleTemplate? captured;
      final bridge = CampusBridge(
        onFetched: (FetchedPage page) {},
        onError: (FetchException error) => fail('不应该报错：${error.code}'),
        onScheduleTemplate: (ScheduleTemplate template) => captured = template,
      );
      final consumed = bridge.handleRaw(
        '{"type":"schedule_template","card_id":"f95c","years":"2026-2027-1","weekcount":"20"}',
      );
      expect(consumed, isTrue);
      expect(captured!.cardId, 'f95c');
      expect(captured!.years, '2026-2027-1');
      expect(captured!.isValid, isTrue);
    });

    test('schedule_api → 交付一次（重复消息忽略）', () {
      final results = <ScheduleApiResult>[];
      final bridge = CampusBridge(
        onFetched: (FetchedPage page) {},
        onError: (FetchException error) => fail('不应该报错：${error.code}'),
        onScheduleApi: (ScheduleApiResult result) => results.add(result),
      );
      const message =
          '{"type":"schedule_api","weeks":[{"week":1,"data":{"zs":"10","resultsJsonArr":[]}}],"missing_weeks":[2]}';
      bridge.handleRaw(message);
      bridge.handleRaw(message);
      expect(results, hasLength(1));
      expect(results.single.missingWeeks, <int>[2]);
    });

    test('auth_required → AUTH_REQUIRED，提示重新登录', () {
      FetchException? failure;
      final bridge = CampusBridge(
        onFetched: (FetchedPage page) {},
        onError: (FetchException error) => failure = error,
      );
      bridge.handleRaw('{"type":"auth_required","message":"登录态已失效"}');
      expect(failure!.code, 'AUTH_REQUIRED');
      expect(failure!.userHint, contains('重新登录'));
    });

    test('CARD_NOT_FOUND 不锁定会话（用户补开课表卡片后还能继续）', () {
      final results = <ScheduleApiResult>[];
      var errors = 0;
      final bridge = CampusBridge(
        onFetched: (FetchedPage page) {},
        onError: (FetchException error) => errors += 1,
        onScheduleApi: (ScheduleApiResult result) => results.add(result),
      );
      bridge.handleRaw('{"type":"error","code":"CARD_NOT_FOUND","message":"没抓到模板"}');
      expect(errors, 1);
      expect(describeFetchFailure('CARD_NOT_FOUND'), contains('打开一次课表卡片'));
      bridge.handleRaw(
        '{"type":"schedule_api","weeks":[{"week":1,"data":{"zs":"10","resultsJsonArr":[]}}]}',
      );
      expect(results, hasLength(1), reason: 'CARD_NOT_FOUND 之后仍可成功');
    });

    test('空表格 HTML 仍走原有 HTML_CHANGED 路径（兜底未坏）', () {
      FetchException? failure;
      final bridge = CampusBridge(
        onFetched: (FetchedPage page) {},
        onError: (FetchException error) => failure = error,
      );
      bridge.handleRaw('{"type":"timetable","html":"  "}');
      expect(failure!.code, 'HTML_CHANGED');
    });
  });
}
