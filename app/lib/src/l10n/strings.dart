// #69：用户可见字符串的单一来源（zh-CN，默认且当前唯一语言）。
//
// 增量策略（不引入 gen-l10n / ARB）：
// - 本轮先迁移 chat_top_bar / campus_entries / provider_card 等 UI 面；
// - 其余文件在 `test/t7_i18n_strings_test.dart` 的 pending 白名单里登记（只许缩小，不许新增）；
// - 将来接 `AppLocalizations.of(context)` 时，把调用点从 `ChatS.x` 换成 `l10n.chat.x` 即可，文案不变。
//
// 注意：本文件与注释是扫描测试唯一的「字符串白名单」。

class ChatS {
  const ChatS._();

  static const String statusTooltip = '状态与联网搜索';
  static const String more = '更多';
  static const String menuLocalSearch = '搜索本地资料（不联网）';
  static const String menuMemory = '记忆';
  static const String menuDiagnostics = '对话诊断';
  static const String menuClear = '清空当前对话历史';
  static const String inputHint = '输入消息…';
  static const String providerPlaceholder = '请选择或添加你的 Provider';

  static String protocolAutoLabel(String providerName) => '当前生效：自动（$providerName）';

  static String modelMigrationNote(String from, String to) => '已把旧模型名 $from 迁移为 $to';
}

class CampusS {
  const CampusS._();

  static const String portal = '教务门户';
  static const String scores = '成绩查询';
  static const String timetable = '我的课表';
  static const String library = '图书馆';
  static const String software = '正版软件';
  static const String captureCacheTooShort = '（过短）';
  static const String captureCacheNever = 'cardId: 尚未抓取过（需要先成功抓取一次课表，之后这里会显示缓存值）';

  static String captureCacheSummary(String maskedCardId, String capturedAtLabel, String years) =>
      'cardId: $maskedCardId（来自上次抓取，缓存于 $capturedAtLabel） · 学期: $years';
}

class SettingsS {
  const SettingsS._();

  static const String addProvider = '添加供应商';
  static const String editProvider = '编辑供应商';
  static const String builtinPresets = '内置预设';
  static const String orCustom = '或自定义';
  static const String fieldName = '名称';
  static const String fieldBaseUrl = 'Base URL';
  static const String fieldApiKey = 'API Key';
  static const String keepBlankIfSaved = '已保存则留空不改';
  static const String fieldModels = '自定义模型（可选，逗号分隔）';
  static const String cancel = '取消';
  static const String save = '保存';
  static const String delete = '删除';
  static const String edit = '编辑';
  static const String alreadyAdded = '已添加';
  static const String selected = '已选择';
  static const String deleteConfirmBody = '只删除本机保存的配置与 Key，不影响账号。';
  static const String keySaved = 'Key 已保存';
  static const String keyBroken = 'Key 密文损坏，请重填';
  static const String keyMissing = '未配置 Key';
  static const String coreUnavailableSuffix = '端侧核心不可用';

  static String saveFailed(Object? failure) => '保存失败：$failure';

  static String deleteProviderTitle(String name) => '删除供应商「$name」？';

  static String providerSubtitle(
    String baseUrl, {
    required bool hasKey,
    required bool broken,
  }) =>
      '$baseUrl · ${!hasKey ? keyMissing : (broken ? keyBroken : keySaved)}';

  static String providerSubtitleCoreUnavailable(String baseUrl) =>
      '$baseUrl · $coreUnavailableSuffix';

  // security M3：清除校务登录态
  static const String clearCampusSession = '清除校务登录态';
  static const String schoolLoginStateTitle = '学校登录态（课表导入获得）';
  static const String schoolLoginStateSubtitle = '当前使用：从校务 WebView 会话取出，只存本机';
  static const String clearCampusSessionConfirm = '清除';
  static const String clearCampusSessionTitle = '清除校务登录态？';
  static const String clearCampusSessionBody =
      '会退出校务登录，下次抓取需重新登录；已入库的课表数据不受影响。';
  static const String clearingCampusSession = '清除中…';
  static const String clearCampusSessionSuccess = '校务登录态已清除';
  static const String clearSessionBadPath = '拒绝清除：会话目录不在数据根下';

  static String clearCampusSessionFailure(Object? error) => '清除失败：$error';
}

class CommonS {
  const CommonS._();

  static const String buildDev = '构建 dev';

  static String buildCommit(String commit) => '构建 $commit';

  static String paperTextureLoadFailed(Object? error) => '纸纹加载失败，退回纯色：$error';
}
