// t67: 跨语言 RAG 向量校验（端侧）。
//
// 唯一事实源是仓库根下的 `docs/rag_vectors.json`（服务端与端侧共享、可 review 的明文 JSON）。
// 服务端修改 `canonical()` / hash 规则时**必须同时更新该文件**，否则这里会红——这是刻意的漂移告警，
// 比"端侧硬编码向量、服务端静默改规则"安全得多。
//
// 假绿防护：`cases` 为空 / 文件缺失 / 字段格式不对都算**失败**，不允许 0 断言通过。

import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/rag_hash.dart';

/// 从仓库根找共享向量文件（`flutter test` 的 cwd 是 `app/`）。
Map<String, dynamic> _loadVectors() {
  const candidates = <String>[
    '../docs/rag_vectors.json',
    'docs/rag_vectors.json',
    'test/fixtures/rag_vectors.json',
  ];
  for (final path in candidates) {
    final file = File(path);
    if (file.existsSync()) {
      final decoded = jsonDecode(file.readAsStringSync());
      if (decoded is Map<String, dynamic>) {
        return decoded;
      }
      fail('$path 顶层不是 JSON 对象');
    }
  }
  fail(
    '找不到共享向量文件 docs/rag_vectors.json（已尝试：${candidates.join(", ")}）。'
    '它必须存在于仓库中——文件缺失意味着"跨语言一致性"没有任何守护。',
  );
}

void main() {
  final vectors = _loadVectors();
  final salt = vectors['salt'];
  final cases = vectors['cases'];

  group('向量文件格式守卫（防假绿）', () {
    test('salt 非空且为字符串', () {
      expect(salt, isA<String>());
      expect((salt as String).trim(), isNotEmpty);
    });

    test('version 存在且是正整数', () {
      final version = vectors['version'];
      expect(version, isA<int>(), reason: 'version 必须存在（文件格式演进用）');
      expect(version as int, greaterThan(0));
    });

    test('cases 非空（空列表会让所有断言静默跳过 → 假绿）', () {
      expect(cases, isA<List<dynamic>>());
      expect(
        (cases as List<dynamic>).length,
        greaterThanOrEqualTo(10),
        reason: '至少 10 条固定向量（t64 任务硬要求）；被误删会让跨语言校验失去意义',
      );
    });

    test('每条 case 字段齐全、hash 是 64 位小写十六进制', () {
      final list = cases as List<dynamic>;
      final hashPattern = RegExp(r'^[0-9a-f]{64}$');
      for (var i = 0; i < list.length; i++) {
        final entry = list[i];
        expect(entry, isA<Map<String, dynamic>>(), reason: 'case[$i] 不是对象');
        final map = entry as Map<String, dynamic>;
        expect(map['raw'], isA<String>(), reason: 'case[$i].raw');
        expect(map['canonical'], isA<String>(), reason: 'case[$i].canonical');
        expect((map['canonical'] as String).isNotEmpty, isTrue, reason: 'case[$i].canonical 不能为空');
        expect(map['hash'], isA<String>(), reason: 'case[$i].hash');
        expect(
          hashPattern.hasMatch(map['hash'] as String),
          isTrue,
          reason: 'case[$i].hash 必须是 64 位小写十六进制：${map['hash']}',
        );
      }
    });
  });

  group('与服务端逐字节一致（读 docs/rag_vectors.json）', () {
    final list = (cases as List<dynamic>).cast<Map<String, dynamic>>();
    for (final entry in list) {
      final raw = entry['raw'] as String;
      final canonical = entry['canonical'] as String;
      final expectedHash = entry['hash'] as String;
      final label = raw.trim().isEmpty ? '<空>' : raw.trim();

      test('canonical($label)', () {
        expect(canonicalIdentifier(raw), canonical);
      });

      test('hash($label)', () {
        expect(hashIdentifier(salt as String, canonical), expectedHash);
      });
    }

    test('rejects 列表：非法输入必须返回 null', () {
      final rejects = vectors['rejects'] as List<dynamic>? ?? <dynamic>[];
      expect(rejects, isNotEmpty, reason: 'rejects 不能为空（空/超长必须被拒绝）');
      for (final raw in rejects.cast<String>()) {
        final preview = raw.length > 20 ? '${raw.substring(0, 20)}…' : raw;
        expect(canonicalIdentifier(raw), isNull, reason: '应拒绝：$preview');
      }
    });
  });

  test('SHA-256 标准向量（NIST）', () {
    expect(sha256Hex(<int>[]), 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    expect(
      sha256Hex('abc'.codeUnits),
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    );
  });

  test('隐私红线：哈希不含标识明文', () {
    const raw = 'https://doi.org/10.1145/1234567.890123';
    final hash = hashRawIdentifier(salt as String, raw)!;
    expect(hash, hasLength(64));
    expect(hash.contains('10.1145'), isFalse);
    expect(hash.contains('1234567'), isFalse);
  });

  group('ASCII-only 契约（不依赖向量文件，防 Unicode 分叉）', () {
    test('U+0130（İ）原样保留，只把 ASCII A–Z 转小写', () {
      // unicode 全映射会把 İ 变成 i + U+0307；ASCII-only 必须保持两个码点不同
      expect(canonicalIdentifier('İSTANBUL'), 'İstanbul');
      expect(canonicalIdentifier('İSTANBUL')!.runes.length, 'İstanbul'.runes.length);
    });

    test('BOM（U+FEFF）不被裁剪（String.trim 会裁掉它）', () {
      final canonical = canonicalIdentifier('\uFEFFhttps://x.com/z');
      expect(canonical, isNotNull);
      expect(canonical!.startsWith('\uFEFF'), isTrue, reason: 'BOM 必须原样保留');
      expect(canonical, '\uFEFFhttps://x.com/z');
    });

    test('只裁 ASCII 空白：\\t/\\n/\\r 裁掉，NBSP 保留', () {
      expect(canonicalIdentifier('\t\n https://x.com/y \r\n'), 'https://x.com/y');
      expect(canonicalIdentifier('\u00A0https://x.com/y'), '\u00A0https://x.com/y');
    });
  });
}
