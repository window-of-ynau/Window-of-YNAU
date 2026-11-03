// t64: 端侧 RAG 哈希与服务端逐字节一致（固定向量来自 Rust 实现）。
//
// 向量生成方式（服务端 Rust `canonical_identifier` + `hash_identifier`，salt = "2026-07-rag-salt"）：
// 见任务报告。**这些数字不能手改**：改了就等于和线上服务端脱钩。

import 'package:flutter_test/flutter_test.dart';
import 'package:yunong/src/core/rag_hash.dart';

const String _salt = '2026-07-rag-salt';

/// `(原始输入, 期望 canonical, 期望 hash)`；NONE 表示应当拒绝。
const List<(String, String?, String?)> _vectors = <(String, String?, String?)>[
  ('  https://Example.com/Paper/?utm_source=x&utm_medium=y#sec1 ', 'https://example.com/paper', 'a17f4d27456618f6c2f22300205c84af5a1cc1e449885898746f0a3e494506e5'),
  ('http://example.com/paper', 'http://example.com/paper', 'bcab6218b7cc3a80939a9f927f9c32f66b480ea41aeaebaaec6476b241b87ab9'),
  ('https://arxiv.org/abs/2401.01234v3', '2401.01234', '0e11837ea129b6ed91621e815c8daf42be57b498839afa4b3a409b3962b16c17'),
  ('arXiv:2401.01234', '2401.01234', '0e11837ea129b6ed91621e815c8daf42be57b498839afa4b3a409b3962b16c17'),
  ('https://doi.org/10.1145/1234567.890123', '10.1145/1234567.890123', '9c3ef973a31ba534fe248c4fb60b0262f6d6533def36b0817b3974a887b84c5f'),
  ('doi:10.1000/xyz', '10.1000/xyz', '9e409b9877f81c09623319f00db44579ed8d579911dd1812e2a67a8328515ec9'),
  ('https://example.com/a/b/', 'https://example.com/a/b', 'f2e7fee175b84b6e172bd35f6ec91a7df5e6335822a773aa9051c73db51fcaf3'),
  ('HTTPS://EXAMPLE.COM/UPPER', 'https://example.com/upper', '2becf4e035732a495e871323e9a25ed5495bfd97690c0ef82b82dde306b6e59d'),
  ('https://example.com/x?utm_campaign=z', 'https://example.com/x', 'f26ed5f001fed2275479c967127f2c254c6bdcd57b4075312244d08fa21b7f0a'),
  ('https://arxiv.org/pdf/2312.99999v12', '2312.99999', 'fd3c87606ddd1808a30194f2fb15e71aff5854351b6fbbca8de5c50af3d8a03c'),
  ('https://example.com/v2/notes', 'https://example.com/v2/notes', 'e8a5dd27129afe4134aaeaeb3a22d685255c68cbf7321ab6e875514c68030347'),
  ('', null, null),
];

void main() {

  test('SHA-256 标准向量（NIST）', () {
    expect(
      sha256Hex(<int>[]),
      'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
    );
    expect(
      sha256Hex('abc'.codeUnits),
      'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    );
  });

  group('与服务端逐字节一致（12 条固定向量）', () {
    for (final (raw, canonical, expectedHash) in _vectors) {
      test('canonical(${raw.isEmpty ? '<空>' : raw})', () {
        expect(canonicalIdentifier(raw), canonical);
      });
      test('hash(${raw.isEmpty ? '<空>' : raw})', () {
        expect(hashRawIdentifier(_salt, raw), expectedHash);
      });
    }
  });

  test('端侧只允许上报 (hash, count)：hash 不含标识明文', () {
    const raw = 'https://doi.org/10.1145/1234567.890123';
    final hash = hashRawIdentifier(_salt, raw)!;
    expect(hash, hasLength(64));
    expect(hash.contains('doi'), isFalse);
    expect(hash.contains('1234567'), isFalse);
    expect(hash, isNot(contains('10.1145')));
  });

  test('hash 对 salt 敏感（跨月不可链接）', () {
    const canonical = 'https://example.com/paper';
    expect(hashIdentifier('2026-07-rag-salt', canonical),
        isNot(hashIdentifier('2026-08-rag-salt', canonical)));
  });

  test('冒烟：5 条"用户引用过的文献"算出 (hash,count)，不上传明文', () {
    // 模拟本地搜索/记忆里用户引用过的 5 篇公开文献（示例数据全部公开标识）。
    const references = <String>[
      'https://arxiv.org/abs/2401.01234v3',
      'https://doi.org/10.1145/1234567.890123',
      'https://example.com/library/rule-2026',
      'https://example.com/notes/?utm_source=inbox',
      'arXiv:2312.99999v12',
    ];
    final counts = <String, int>{};
    for (final reference in references) {
      final hash = hashRawIdentifier(_salt, reference);
      if (hash != null) {
        counts[hash] = (counts[hash] ?? 0) + 1;
      }
    }
    expect(counts, hasLength(5));
    // 只允许 (hash, count) 形态：键是 64 位十六进制，没有任何明文
    for (final entry in counts.entries) {
      expect(entry.key, hasLength(64));
      expect(RegExp(r'^[0-9a-f]{64}$').hasMatch(entry.key), isTrue);
      expect(entry.value, 1);
    }
    // ignore: avoid_print
    print('smoke (hash,count) = ${counts.entries.map((e) => '${e.key.substring(0, 12)}…:${e.value}').join(', ')}');
  });
}
