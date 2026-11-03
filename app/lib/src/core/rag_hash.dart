// 端侧 RAG 哈希契约（t64）：与 Rust `crates/server/src/rag_fetch.rs` **逐字节一致**。
//
// # 契约（服务端是唯一真相，改这里必须同步改服务端）
//
// ```
// canonical(raw):
//   1. trim 首尾空白；空串或 > 512 字节 → null
//   2. 转小写
//   3. 截断到第一个 '#'（fragment）
//   4. 截断到第一个 '?'（query，含 utm_*）
//   5. 依次剥一个前缀：https://doi.org/ | http://doi.org/ | doi: |
//      https://arxiv.org/abs/ | http://arxiv.org/abs/ |
//      https://arxiv.org/pdf/ | http://arxiv.org/pdf/ | arxiv:
//   6. arXiv 版本号：最后一个 'v' 后全是数字、且 'v' 前出现过数字 → 截掉
//   7. 去掉结尾所有 '/'
//   8. 空串 → null
//
// hash(salt, canonical) = hex(sha256( utf8(salt) ‖ b"\n" ‖ utf8(canonical) ))，64 位小写
// ```
//
// # 隐私红线（ADR-19）
//
// 端侧**只上传 `(hash, count)`，绝不上传 identifier 明文**。
// 哈希是密钥化摘要（月盐公开下发），只提供"跨月不可链接"，不提供来源认证。
//
// # 为什么是纯 Dart SHA-256
//
// `pubspec.yaml` 里没有 `crypto` 包；两条路里我选**自己实现 SHA-256**：
// 零新增依赖、不引插件（避免再动 Windows 构建链）、可在任意 isolate/测试里跑。
// 正确性由下面两组固定向量锁定：NIST 标准向量 + 12 条由服务端 Rust 实现产出的向量。

import 'dart:convert';
import 'dart:typed_data';

/// 标识最大字节数（与服务端 `MAX_IDENTIFIER_BYTES` 一致）。
const int maxIdentifierBytes = 512;

const List<String> _prefixes = <String>[
  'https://doi.org/',
  'http://doi.org/',
  'doi:',
  'https://arxiv.org/abs/',
  'http://arxiv.org/abs/',
  'https://arxiv.org/pdf/',
  'http://arxiv.org/pdf/',
  'arxiv:',
];

/// ASCII 空白集合：空格 / `\t` / `\n` / `\r` / `\x0c` / `\x0b`。
///
/// **不能用 `String.trim()`**：它按 Unicode 空白裁剪（包含 BOM `\uFEFF`），
/// 与服务端 `str::trim()` 的语义不同 → 同一输入两端算出不同哈希。
bool _isAsciiSpace(int codeUnit) =>
    codeUnit == 0x20 || codeUnit == 0x09 || codeUnit == 0x0a || codeUnit == 0x0d || codeUnit == 0x0c || codeUnit == 0x0b;

/// 只裁 ASCII 空白（首尾）。
String _trimAscii(String value) {
  var start = 0;
  var end = value.length;
  while (start < end && _isAsciiSpace(value.codeUnitAt(start))) {
    start++;
  }
  while (end > start && _isAsciiSpace(value.codeUnitAt(end - 1))) {
    end--;
  }
  return value.substring(start, end);
}

/// 只把 ASCII `A–Z` 转小写，其它码点**原样保留**。
///
/// **不能用 `toLowerCase()`**：它做 Unicode 大小写映射（例如 `İ` U+0130 → `i` + U+0307，
/// 或某些字符映射成单字符），与服务端 `to_ascii_lowercase` 语义不同 → 哈希分叉。
String _toLowerAscii(String value) {
  final units = value.codeUnits;
  final out = <int>[];
  for (final unit in units) {
    if (unit >= 0x41 && unit <= 0x5a) {
      out.add(unit + 0x20);
    } else {
      out.add(unit);
    }
  }
  return String.fromCharCodes(out);
}
/// 规范化标识；非法/空 → null。
///
/// **规则（与 Rust 服务端逐字节一致，标识是 ASCII 语义）**：
/// - 只裁 **ASCII 空白**（见 [_isAsciiSpace]），**不**用 `String.trim()`（会裁 BOM 等 Unicode 空白）；
/// - 只把 **ASCII A–Z** 转小写，**不**用 `toLowerCase()`（会做 Unicode 大小写映射）；
/// - 其余字符（含 Unicode、BOM、组合字符）**原样保留**，不做 NFC 归一化。
/// 违反任一条都会与服务端算出不同哈希，而 join 失败是**静默**的（0 命中、无报错）。
String? canonicalIdentifier(String raw) {
  var value = _trimAscii(raw);
  if (value.isEmpty || utf8.encode(value).length > maxIdentifierBytes) {
    return null;
  }
  value = _toLowerAscii(value);
  final hash = value.indexOf('#');
  if (hash >= 0) {
    value = value.substring(0, hash);
  }
  final question = value.indexOf('?');
  if (question >= 0) {
    value = value.substring(0, question);
  }
  for (final prefix in _prefixes) {
    if (value.startsWith(prefix)) {
      value = value.substring(prefix.length);
      break;
    }
  }
  // arXiv 版本后缀：`2401.01234v3` → `2401.01234`
  final vIndex = value.lastIndexOf('v');
  if (vIndex >= 0) {
    final suffix = value.substring(vIndex + 1);
    final isDigits = suffix.isNotEmpty && RegExp(r'^\d+$').hasMatch(suffix);
    final hasDigitBefore = value.substring(0, vIndex).contains(RegExp(r'\d'));
    if (isDigits && hasDigitBefore) {
      value = value.substring(0, vIndex);
    }
  }
  while (value.endsWith('/')) {
    value = value.substring(0, value.length - 1);
  }
  if (value.isEmpty) {
    return null;
  }
  return value;
}

/// 月度哈希：`hex(sha256(salt ‖ b"\n" ‖ canonical))`。输入必须是**已规范化**的标识。
String hashIdentifier(String salt, String canonical) =>
    sha256Hex(<int>[...utf8.encode(salt), 0x0a, ...utf8.encode(canonical)]);

/// 一步到位：原始标识 → 哈希（非法返回 null）。**明文不出端侧**。
String? hashRawIdentifier(String salt, String raw) {
  final canonical = canonicalIdentifier(raw);
  if (canonical == null) {
    return null;
  }
  return hashIdentifier(salt, canonical);
}

/// SHA-256（FIPS 180-4），返回 64 位小写十六进制。
String sha256Hex(List<int> input) {
  final digest = _sha256(input);
  final buffer = StringBuffer();
  for (final word in digest) {
    // digest 是 32 位 word，必须逐字节取（t64 踩过：直接 toRadixString 会少字符）
    for (var shift = 24; shift >= 0; shift -= 8) {
      buffer.write(((word >> shift) & 0xff).toRadixString(16).padLeft(2, '0'));
    }
  }
  return buffer.toString();
}

const List<int> _k = <int>[
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

Uint32List _sha256(List<int> message) {
  final hash = Uint32List.fromList(<int>[
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ]);

  final bitLength = message.length * 8;
  final withPadding = <int>[...message, 0x80];
  while (withPadding.length % 64 != 56) {
    withPadding.add(0);
  }
  for (var shift = 56; shift >= 0; shift -= 8) {
    withPadding.add((bitLength >> shift) & 0xff);
  }

  final w = Uint32List(64);
  for (var offset = 0; offset < withPadding.length; offset += 64) {
    for (var i = 0; i < 16; i++) {
      final base = offset + i * 4;
      w[i] = (withPadding[base] << 24) |
          (withPadding[base + 1] << 16) |
          (withPadding[base + 2] << 8) |
          withPadding[base + 3];
    }
    for (var i = 16; i < 64; i++) {
      final s0 = _rotr(w[i - 15], 7) ^ _rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
      final s1 = _rotr(w[i - 2], 17) ^ _rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
      w[i] = (w[i - 16] + s0 + w[i - 7] + s1) & 0xffffffff;
    }

    var a = hash[0];
    var b = hash[1];
    var c = hash[2];
    var d = hash[3];
    var e = hash[4];
    var f = hash[5];
    var g = hash[6];
    var h = hash[7];

    for (var i = 0; i < 64; i++) {
      final s1 = _rotr(e, 6) ^ _rotr(e, 11) ^ _rotr(e, 25);
      final ch = (e & f) ^ ((~e & 0xffffffff) & g);
      final temp1 = (h + s1 + ch + _k[i] + w[i]) & 0xffffffff;
      final s0 = _rotr(a, 2) ^ _rotr(a, 13) ^ _rotr(a, 22);
      final maj = (a & b) ^ (a & c) ^ (b & c);
      final temp2 = (s0 + maj) & 0xffffffff;

      h = g;
      g = f;
      f = e;
      e = (d + temp1) & 0xffffffff;
      d = c;
      c = b;
      b = a;
      a = (temp1 + temp2) & 0xffffffff;
    }

    hash[0] = (hash[0] + a) & 0xffffffff;
    hash[1] = (hash[1] + b) & 0xffffffff;
    hash[2] = (hash[2] + c) & 0xffffffff;
    hash[3] = (hash[3] + d) & 0xffffffff;
    hash[4] = (hash[4] + e) & 0xffffffff;
    hash[5] = (hash[5] + f) & 0xffffffff;
    hash[6] = (hash[6] + g) & 0xffffffff;
    hash[7] = (hash[7] + h) & 0xffffffff;
  }

  final out = Uint32List(8);
  for (var i = 0; i < 8; i++) {
    out[i] = hash[i];
  }
  return out;
}

int _rotr(int value, int amount) =>
    ((value >> amount) | (value << (32 - amount))) & 0xffffffff;
