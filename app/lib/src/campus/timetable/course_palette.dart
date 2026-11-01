// 课程配色：同一门课在冷启动 / 切周 / 改窗口后永远是同一个颜色。
//
// 为什么不是朴素的 `palette[hash(name) % n]`：
// 对真实课程名实测（FNV-1a 32 位，n=9），**9 门课里 5 门撞在同一个桶**
// （高等数学I / 数据结构 / 计算机网络 / 概率论与数理统计 / 线性代数）。
// "同课同色"成立，但"不同课不同色"失效，一张课表里 5 门课一个颜色，
// 扫读价值直接归零。
//
// 所以改成「按课程名 ordinal 排序后贪心占槽」：
//   1. 取**本学期全部**课程名（不是本周可见的课）→ 切周不会引起换色；
//   2. 名字按 UTF-16 code unit 排序（`String.compareTo` 就是该语义，与 locale 无关）；
//   3. 每门课从 `hash % N` 起向后线性探测，取第一个没被占的槽位；
//   4. 槽位用尽（本学期 > 9 门课）才复用，复用者加虚线边框区分。
//
// 稳定性：同一份课表数据 → 每次启动、每次切周、每次改窗口大小，颜色都不变。
// 代价：课程**集合**增删时，排序在后面的课可能被挤到别的槽位而换色。
// （纯哈希可以换来"永不换色"，但要接受撞色；这里选了"不相撞"。）

import 'package:flutter/material.dart';

/// 一套课程配色：`container` 是块底，`ink` 是课名，`inkSoft` 是教室/节次，
/// `border` 只用来把块从白底上勾出来。
///
/// 全部按"浅底深字"选过对比度（WCAG 2.1 相对亮度实算）：
/// 课名 ≥ 7.4:1（AAA），次要信息 ≥ 4.6:1（AA）。
@immutable
class CourseTint {
  const CourseTint({
    required this.container,
    required this.ink,
    required this.inkSoft,
    required this.border,
    this.deep = false,
  });

  final Color container;
  final Color ink;
  final Color inkSoft;
  final Color border;

  /// 第二轮的同一色相（本学期课程数超过色板容量时用）：靠深浅区分，不是同色。
  final bool deep;

  /// 加深一档：底色/描边往 ink 压 16%/26%，课名颜色不变（对比度只会更高，实测 ≥7.0:1）。
  /// 压得够深才看得出"同色相两档"，压 10% 时分不清（实测对比截图）。
  CourseTint get deeper => CourseTint(
        container: Color.alphaBlend(ink.withValues(alpha: 0.16), container),
        ink: ink,
        inkSoft: inkSoft,
        border: Color.alphaBlend(ink.withValues(alpha: 0.26), border),
        deep: true,
      );
}

/// 浅色主题的 11 色板。**故意不放纯红/纯琥珀**：红色留给"解析失败"，
/// 琥珀留给"未确认学期开始日"，否则用户会把一门课看成报错。
///
/// 11 色 × 2 轮（第二轮加深一档）= 22 个槽位：常见学期（6~14 门）不会撞色。
const List<CourseTint> kCourseTintsLight = <CourseTint>[
  CourseTint(
    container: Color(0xFFE8F1FE),
    ink: Color(0xFF123C7A),
    inkSoft: Color(0xFF3D6094),
    border: Color(0xFFCAD8EC),
  ),
  CourseTint(
    container: Color(0xFFDFF4F3),
    ink: Color(0xFF0B4B4A),
    inkSoft: Color(0xFF356D6C),
    border: Color(0xFFC1DCDB),
  ),
  CourseTint(
    container: Color(0xFFE0F3E8),
    ink: Color(0xFF12503A),
    inkSoft: Color(0xFF3B715D),
    border: Color(0xFFC3DCD0),
  ),
  CourseTint(
    container: Color(0xFFE6F4DF),
    ink: Color(0xFF2A4F12),
    inkSoft: Color(0xFF50703B),
    border: Color(0xFFCCDDC2),
  ),
  CourseTint(
    container: Color(0xFFF1F3D9),
    ink: Color(0xFF43480F),
    inkSoft: Color(0xFF6E7531),
    border: Color(0xFFD9DBBD),
  ),
  CourseTint(
    container: Color(0xFFFDEBDD),
    ink: Color(0xFF7A3A06),
    inkSoft: Color(0xFF945D31),
    border: Color(0xFFEBD2BF),
  ),
  CourseTint(
    container: Color(0xFFFBE4EE),
    ink: Color(0xFF7A1F45),
    inkSoft: Color(0xFF944667),
    border: Color(0xFFE9C8D6),
  ),
  CourseTint(
    container: Color(0xFFEDE7FB),
    ink: Color(0xFF432C7A),
    inkSoft: Color(0xFF655194),
    border: Color(0xFFD5CDE9),
  ),
  CourseTint(
    container: Color(0xFFE6E9FB),
    ink: Color(0xFF2A3470),
    inkSoft: Color(0xFF50588C),
    border: Color(0xFFCCD0E8),
  ),
  CourseTint(
    container: Color(0xFFE1F2FB),
    ink: Color(0xFF0D4462),
    inkSoft: Color(0xFF376781),
    border: Color(0xFFC3DAE6),
  ),
  CourseTint(
    container: Color(0xFFE8EDF3),
    ink: Color(0xFF2F3E4F),
    inkSoft: Color(0xFF546170),
    border: Color(0xFFCED4DC),
  ),
];

/// 深色主题：同 9 个色相，明度反转（深底浅字），对比度 9.1~10.3 / 6.4~7.2。
const List<CourseTint> kCourseTintsDark = <CourseTint>[
  CourseTint(
    container: Color(0xFF16283F),
    ink: Color(0xFFBBD6FF),
    inkSoft: Color(0xFF9AB3D9),
    border: Color(0xFF3A4E69),
  ),
  CourseTint(
    container: Color(0xFF12302F),
    ink: Color(0xFFA8DEDC),
    inkSoft: Color(0xFF8ABBB9),
    border: Color(0xFF335655),
  ),
  CourseTint(
    container: Color(0xFF13302A),
    ink: Color(0xFFA9DFC7),
    inkSoft: Color(0xFF8BBCA8),
    border: Color(0xFF34564D),
  ),
  CourseTint(
    container: Color(0xFF1D2E15),
    ink: Color(0xFFC4E3AB),
    inkSoft: Color(0xFFA3BF8D),
    border: Color(0xFF425636),
  ),
  CourseTint(
    container: Color(0xFF2C2C15),
    ink: Color(0xFFDCDCA6),
    inkSoft: Color(0xFFC1C18F),
    border: Color(0xFF535335),
  ),
  CourseTint(
    container: Color(0xFF33220F),
    ink: Color(0xFFF0C79A),
    inkSoft: Color(0xFFCAA67E),
    border: Color(0xFF5D462E),
  ),
  CourseTint(
    container: Color(0xFF331C25),
    ink: Color(0xFFF2B7CF),
    inkSoft: Color(0xFFCC98AD),
    border: Color(0xFF5D3E4A),
  ),
  CourseTint(
    container: Color(0xFF261E3A),
    ink: Color(0xFFCFC0F5),
    inkSoft: Color(0xFFADA0D0),
    border: Color(0xFF4B4263),
  ),
  CourseTint(
    container: Color(0xFF212540),
    ink: Color(0xFFC2C8F2),
    inkSoft: Color(0xFFA2A7CE),
    border: Color(0xFF444967),
  ),
  CourseTint(
    container: Color(0xFF0F2C38),
    ink: Color(0xFFA6D7EF),
    inkSoft: Color(0xFF89B4CB),
    border: Color(0xFF2E4E59),
  ),
  CourseTint(
    container: Color(0xFF242B33),
    ink: Color(0xFFC3CFDD),
    inkSoft: Color(0xFFA4AFBC),
    border: Color(0xFF414A55),
  ),
];

/// FNV-1a 32 位。**不要换成 `String.hashCode`**：Dart 只保证同进程内稳定，
/// 不保证跨版本/跨平台一致，换一次 SDK 课表就整体换色。
int courseNameFingerprint(String name) {
  int hash = 0x811C9DC5;
  for (final int unit in name.codeUnits) {
    hash ^= unit;
    hash = (hash * 0x01000193) & 0xFFFFFFFF;
  }
  return hash;
}

/// 本学期「课程名 → 槽位」映射。
///
/// 输入应当是**整学期**的课程名集合（不是本周可见课），否则切周会换色。
///
/// 槽位分配分两轮，**先占本色再占加深档**：
///   1. 第一轮只用 `0..N-1`（色板本色，颜色最鲜）；
///   2. 第一轮占满后（本学期 > N 门课）才用 `N..2N-1`（同色相加深一档）。
/// 这样 6~11 门课时整张课表全是本色（不会因为色板变大就变灰），
/// 12~22 门课时才出现"同色相深浅不同"的两块。
Map<String, int> assignCourseTints(Iterable<String> names) {
  const int base = 11; // 与 kCourseTintsLight/Dark 的长度一致（由测试兜底）
  final List<String> ordered = names.toSet().toList()..sort();
  final Set<int> used = <int>{};
  final Map<String, int> result = <String, int>{};
  for (final String name in ordered) {
    final int start = courseNameFingerprint(name) % base;
    int picked = -1;
    for (int step = 0; step < base; step++) {
      final int candidate = (start + step) % base;
      if (!used.contains(candidate)) {
        picked = candidate;
        break;
      }
    }
    if (picked < 0) {
      for (int step = 0; step < base; step++) {
        final int candidate = base + (start + step) % base;
        if (!used.contains(candidate)) {
          picked = candidate;
          break;
        }
      }
    }
    if (picked < 0) {
      // 超过 2N 门课：只能复用（此时同色不可避免，详情面板里仍可区分）
      picked = start;
    }
    used.add(picked);
    result[name] = picked;
  }
  return result;
}

/// 取某门课在当前亮度下的一套颜色（自动处理"第二轮加深"）。
CourseTint courseTintOf(
  BuildContext context,
  Map<String, int> assignment,
  String courseName,
) {
  final List<CourseTint> palette = Theme.of(context).brightness == Brightness.dark
      ? kCourseTintsDark
      : kCourseTintsLight;
  final int slot = assignment[courseName] ?? 0;
  final CourseTint base = palette[slot % palette.length];
  return slot >= palette.length ? base.deeper : base;
}
