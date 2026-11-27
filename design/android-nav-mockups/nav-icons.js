/* 云农之窗 · 导航图标单一来源（#84 定稿）
 *
 * 视觉稿用 Flutter 自带 MaterialIcons-regular.otf 渲染，保证「视觉稿 == 实现」：
 * 每个槽位的 codepoint 与 Flutter SDK `IconData` 完全一致（见 design/android-nav-icons.md §2）。
 * 用法：<link> 到 CSS 的 @font-face（family: 'MI'），本文件只输出 <span class="mi">。
 * 图标颜色一律继承父级 color → 由主题 token 控制，不在此处写死颜色。
 */
window.NAV = (function () {
  // 槽位 → MaterialIcons codepoint（与 flutter/packages/flutter/lib/src/material/icons.dart 对齐）
  var CP = {
    // —— 6 个主入口（#84 指定：课表 / 成绩 / 规章 / 校园生活 / 对话 / 设置）——
    chat: 'e155',        // Icons.chat_bubble_outline
    timetable: 'f051f',  // Icons.calendar_month_outlined
    scores: 'ee95',      // Icons.assessment_outlined
    regulations: 'f1c2', // Icons.menu_book_outlined
    campus: 'f0db',      // Icons.groups_outlined
    settings: 'f36e',    // Icons.settings_outlined
    // —— 现有 CampusEntry 其余入口（同一语言，避免 #84「图标全同」在它们身上复发）——
    portal: 'ee32',      // Icons.account_balance_outlined
    library: 'f181',     // Icons.local_library_outlined
    software: 'f47c'     // Icons.verified_outlined
  };
  var NAME = {
    chat: '对话', timetable: '课表', scores: '成绩', regulations: '规章',
    campus: '校园生活', settings: '设置',
    portal: '教务门户', library: '图书馆', software: '正版软件'
  };
  // 语义理由（给表格用）
  var WHY = {
    chat: '圆角对话气泡 + 左下尾，唯一「说话」轮廓',
    timetable: '日历网格 + 表头，读作「周课表」',
    scores: '文档 + 三根升序柱，读作「成绩单 / 统计」',
    regulations: '摊开书本，读作「规章制度汇编」',
    campus: '三人组，读作「社团 / 校园生活」',
    settings: '齿轮，通用设置语义',
    portal: '三角顶 + 立柱 + 台基，读作「教务大厅」',
    library: '人头 + 摊开书，读作「图书馆」',
    software: '印章轮廓 + 对勾，读作「正版 / 已验证」'
  };
  // 界面操作图标（非导航入口，但同一 20dp 规范）
  var CHROME = { menu: 'e3dc', menu_open: 'e3de', close: 'e16a' };

  function glyph(cp, size) {
    var ch = String.fromCodePoint(parseInt(cp, 16));
    return '<span class="mi" style="font-size:' + size + 'px">' + ch + '</span>';
  }
  function icon(id, size) {
    return glyph(CP[id] || CHROME[id], size || 20);
  }
  return { CP: CP, NAME: NAME, WHY: WHY, CHROME: CHROME, icon: icon, glyph: glyph };
})();
