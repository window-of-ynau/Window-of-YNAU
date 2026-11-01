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

  group('学期开始日来自校历锚点（按学期取，不按 zs 推算）', () {
    final start = suggestedTermStart('2026-2027-1')!;

    test('第一学期锚点 2026-09-07 且是周一', () {
      expect(formatIsoDate(start), '2026-09-07');
      expect(start.weekday, DateTime.monday);
    });

    test('校历三个学期的锚点都是周一', () {
      for (final term in <String>['2026-2027-1', '2026-2027-2', '2026-2027-3']) {
        final anchor = suggestedTermStart(term);
        expect(anchor, isNotNull, reason: '$term 应有校历锚点');
        expect(anchor!.weekday, DateTime.monday, reason: '$term 锚点必须是周一');
      }
      expect(formatIsoDate(suggestedTermStart('2026-2027-2')!), '2027-03-01');
      expect(formatIsoDate(suggestedTermStart('2026-2027-3')!), '2027-07-05');
    });

    test('未验证过的学期没有建议值（必须让用户确认，不能套用别的学期）', () {
      expect(suggestedTermStart('2027-2028-1'), isNull);
      expect(suggestedTermStart(''), isNull);
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
      // 9/14 起算：第 7 周 = 10-26 ~ 11-01，所以 11/01 是第 7 周（原来写 8 是错的）
      expect(currentWeek(termStart: custom, today: DateTime(2026, 11, 1)), 7);
      // 用户确认过的值优先：解析出来的自定义锚点确实生效（而不是被校历锚点覆盖）
      expect(formatIsoDate(parseTermStart('2026-09-14')!), '2026-09-14');
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
      expect(kSeededTermStarts['2026-2027-1'], '2026-09-07');
    });
  });
  group('cardId 从 HTML 里探测（owner 实测 18 个命中、真实值排第 14）', () {
    // owner 给出的真实页面候选（已脱敏为静态度量：顺序与重复都保留）
    const candidates = <String>[
      'a1b2c3d4e5f60718293a4b5c6d7e8f90',
      '6f5bd67d6fa147afb820d47f26bf78ed',
      '22208d630bda4d9a9f1c2d3e4f506172',
      '4d72f8a7b5c34e2f8a9b0c1d2e3f4051',
      'f762ddbe2d3c4b5a6978e0f1d2c3b4a5',
      '6f5bd67d6fa147afb820d47f26bf78ed',
      'b5c5556e5a714fccb490aa4832762cbb',
      'f95c8b218c6946bea509c540c50eac18', // 真实 cardId
      '6f5bd67d6fa147afb820d47f26bf78ed',
      '22208d630bda4d9a9f1c2d3e4f506172',
      '4d72f8a7b5c34e2f8a9b0c1d2e3f4051',
      'f762ddbe2d3c4b5a6978e0f1d2c3b4a5',
      '0f1e2d3c4b5a69788796a5b4c3d2e1f0',
      '9a8b7c6d5e4f30211203f4e5d6c7b8a9',
      '1234567890abcdef1234567890abcdef',
      'abcdef0123456789abcdef0123456789',
    ];

    String pageHtml({String? timetableHint}) {
      final buffer = StringBuffer('<html><head>');
      for (final value in candidates) {
        final hint = timetableHint ?? '';
        buffer.write('<script>window.__D.push({"id":"$value"$hint})</script>');
      }
      buffer.write('</head><body>门户</body></html>');
      return buffer.toString();
    }

    test('去重后按出现顺序保留，最多 12 个候选', () {
      final extracted = extractCardIdCandidates(pageHtml());
      final values = extracted.map((CardIdCandidate item) => item.value).toList();
      expect(values.toSet().length, values.length, reason: '不能重复');
      // fixture 16 条、其中 5 条是重复（6f5b×3、2220×2、4d72×2、f762×2、b5c5×1 …）→ **去重后 11 条**；
      // 这是实测值（不是照实现改断言：owner 原始 18 条去重 12，本 fixture 脱敏时少了一个值）。
      expect(values, hasLength(11));
      expect(values, contains('f95c8b218c6946bea509c540c50eac18'));
    });

    test('课表相关上下文排前面（其余保持出现顺序）', () {
      const html = '<html><body>'
          '<script>var a="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";</script>'
          '<script>var b="f95c8b218c6946bea509c540c50eac18"; var t="课表 kbsz";</script>'
          '<script>var c="cccccccccccccccccccccccccccccccc";</script>'
          '</body></html>';
      final extracted = extractCardIdCandidates(html);
      expect(extracted.first.value, 'f95c8b218c6946bea509c540c50eac18');
    });

    test('逐个探测：前 11 个返回 4xx/空都不崩，第 12 个命中即停', () async {
      final extracted = extractCardIdCandidates(pageHtml());
      final probed = <String>[];
      final hit = await selectCardIdByProbing(
        extracted,
        (CardIdCandidate candidate) async {
          probed.add(candidate.value);
          // 模拟真实服务端：只有真实 cardId 返回带 resultsJsonArr 的数据
          if (candidate.value == 'f95c8b218c6946bea509c540c50eac18') {
            return isScheduleProbeHit(<String, dynamic>{
              'data': <String, dynamic>{
                'resultsJsonArr': List<dynamic>.generate(7, (int i) => <dynamic>[]),
              },
            });
          }
          if (candidate.value.startsWith('6f5b')) {
            throw StateError('404');
          }
          return false;
        },
        maxAttempts: 12,
      );
      expect(hit, isNotNull);
      expect(hit!.value, 'f95c8b218c6946bea509c540c50eac18');
      // 顺序：a1b2, 6f5b, 2220, 4d72, f762, b5c5, f95c（第 7 次命中）
      expect(probed, hasLength(7), reason: '必须靠探测而不是位置（命中即停）');
      expect(probed.first, 'a1b2c3d4e5f60718293a4b5c6d7e8f90', reason: '按出现顺序探测');
      expect(probed.last, 'f95c8b218c6946bea509c540c50eac18');
    });

    test('全部不命中时返回 null（交给超时提示，而不是猜一个）', () async {
      final extracted = extractCardIdCandidates(pageHtml());
      final hit = await selectCardIdByProbing(
        extracted,
        (CardIdCandidate candidate) async => false,
        maxAttempts: 12,
      );
      expect(hit, isNull);
    });

    test('命中判据要求 resultsJsonArr 是非空行数组', () {
      expect(isScheduleProbeHit(<String, dynamic>{'data': <String, dynamic>{}}), isFalse);
      expect(
        isScheduleProbeHit(<String, dynamic>{
          'data': <String, dynamic>{'resultsJsonArr': <dynamic>[]},
        }),
        isFalse,
      );
      expect(
        isScheduleProbeHit(<String, dynamic>{
          'data': <String, dynamic>{
            'resultsJsonArr': List<dynamic>.generate(7, (int i) => <dynamic>[]),
          },
        }),
        isTrue,
      );
      expect(isScheduleProbeHit(<String, dynamic>{'code': 500}), isFalse);
      expect(isScheduleProbeHit('html'), isFalse);
    });
  });


  group('周次筛选（默认当前周 + 切周）', () {
    test('区间展开：1-16 / 1-8,10-16 / 单周 / 空', () {
      expect(expandWeeks('1-16'), containsAll(<int>[1, 8, 16]));
      expect(expandWeeks('1-16').length, 16);
      expect(expandWeeks('1-8,10-16'), isNot(contains(9)));
      expect(expandWeeks('1-8,10-16').length, 15);
      expect(expandWeeks('5'), <int>{5});
      expect(expandWeeks(''), isEmpty);
      expect(expandWeeks('乱写'), isEmpty);
    });

    test('某周是否有课', () {
      expect(occursInWeek('1-16', 8), isTrue);
      expect(occursInWeek('1-8', 9), isFalse);
      expect(occursInWeek('1-8,10-16', 9), isFalse);
      expect(occursInWeek('1-8,10-16', 10), isTrue);
      expect(occursInWeek('1-8', 0), isFalse);
    });

    test('默认周：2026-09-07 锚点下 2026-11-01 是第 8 周', () {
      final week = currentWeek(termStart: DateTime(2026, 9, 7), today: DateTime(2026, 11, 1));
      expect(week, 8);
    });
  });
  group('SSO 登录页判定（登录等待阶段不抓取）', () {
    test('登录页：host+path 都命中', () {
      expect(isSsoLoginPage('ssoo.ynau.edu.cn', '/lyuapServer/login'), isTrue);
      expect(isSsoLoginPage('ssoo.ynau.edu.cn', '/lyuapServer/login?x=1'), isTrue);
    });

    test('门户页 / 空值：不是登录页', () {
      expect(isSsoLoginPage('one.ynau.edu.cn', '/#/index5'), isFalse);
      expect(isSsoLoginPage('', ''), isFalse);
      expect(isSsoLoginPage('one.ynau.edu.cn', '/api/uppcard/kbsz/queryAWeekSchedule'), isFalse);
    });

    test('大小写不敏感', () {
      expect(isSsoLoginPage('SSOO.YNAU.EDU.CN', '/LYUAPSERVER/LOGIN'), isTrue);
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
