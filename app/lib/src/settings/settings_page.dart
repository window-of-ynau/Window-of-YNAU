// 设置页（批次 3）：外观是主角——三张主题预览卡（主题即决定明暗，不再单独提供明暗开关）。
//
// 交互约定：
// - 点预览卡 = 立即换主题（不重启、不弹确认）；
// - 「玻璃强度」在批次 2 接（B 的真模糊与降级做完再放出来），现在不显示半成品；
// - 所有颜色/圆角/字号都来自 `context.tokens`，页面本身不写死任何视觉值。

import 'package:flutter/material.dart';

import '../chat/chat_settings_section.dart';
import '../core/build_info.dart';
import '../core/auth_service.dart';
import '../core/secret_service.dart';
import '../core/app_paths.dart';
import 'provider_card.dart';
import '../core/provider_store.dart';

import '../theme/theme_controller.dart';
import '../theme/glass_panel.dart';
import '../theme/themes.dart';
import '../theme/yunong_tokens.dart';

class SettingsPage extends StatelessWidget {
  const SettingsPage(
      {super.key,
      required this.controller,
      this.chatSettings,
      this.authService,
      this.secretService,
      this.providerStore,
      this.keyValidator,
      this.onKeySaved,
      this.version = '0.9.0（W2b）'});


  /// 主题控制器（设置页改主题立即生效）
  final ThemeController controller;
  /// 对话设置的共享状态源（与对话页同一实例）：本页改了对话页立即生效
  final ChatSettingsController? chatSettings;

  /// 账号能力（t88 下沉）：给了它，设置页里的对话设置就能登录/退出套餐
  final AuthService? authService;

  /// Key 能力（t88 下沉）：给了它，保存 Key 的校验/保存/回读都在面板内完成
  final SecretService? secretService;

  /// 供应商列表（t115）：没传就建一个内存版（测试/无核心时也能渲染页面）。
  final ProviderStore? providerStore;

  /// 没传 store 时的兜底（无核心/测试环境也能渲染供应商卡）。
  static final ProviderStore _fallbackProviders = ProviderStore();
  final Future<String?> Function(String apiKey)? keyValidator;
  final void Function(SecretStatus status)? onKeySaved;
  final String version;

  /// t153：是否显示调试/诊断入口（null = 跟随构建模式：debug 显示、release 隐藏）。
  ///
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: controller,
      builder: (BuildContext context, Widget? _) {
        final YunongTokens t = context.tokens;
        return Scaffold(
          appBar: AppBar(
            title: const Text('设置'),
            backgroundColor: t.glass == null ? t.canvas : Colors.transparent,
            flexibleSpace: t.glass == null
                ? null
                : const GlassPanel(
                    role: GlassRole.appBar,
                    borderRadius: BorderRadius.zero,
                    child: SizedBox.expand(),
                  ),
          ),
          body: ListView(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 32),
            children: <Widget>[
              // t115：设置页只负责**供应商**（唯一填 Key 的地方）；模型/effort 在对话页。
              _Group(
                title: '供应商',
                children: <Widget>[
                  ProviderCard(store: providerStore ?? _fallbackProviders, secrets: secretService),
                ],
              ),
              // t132：账号分区（结构先立起来；第三/四条是**显式占位**，标「即将支持」，
              // 不做成点了没反应的假按钮）。
              const _Group(
                title: '账号',
                children: <Widget>[
                  const _Row(
                    icon: Icons.school_outlined,
                    title: '学校登录态（课表导入获得）',
                    subtitle: '当前使用：从校务 WebView 会话取出，只存本机',
                    trailing: Icon(Icons.check_circle_outline, size: 18),
                  ),
                  const _Row(
                    icon: Icons.person_outline,
                    title: '云农账号（自有账号体系）',
                    subtitle: '即将支持 · 用云农账号登录后同步消息与记忆',
                    trailing: Icon(Icons.schedule, size: 18),
                  ),
                  const _Row(
                    icon: Icons.card_membership_outlined,
                    title: '套餐登录',
                    subtitle: '即将支持 · 现可在对话页「模型选择器 → 套餐通道」登录',
                    trailing: Icon(Icons.schedule, size: 18),
                  ),
                ],
              ),
              // t130：设置页保留「供应商 / 账号 / 外观 / 隐私与记忆 / 关于」。
              // 「对话」组已删除：通道、模型、effort、套餐登录全部在对话页的模型选择器里。
              _Group(
                title: '外观',
                children: <Widget>[
                  Padding(
                    padding: const EdgeInsets.fromLTRB(14, 12, 14, 14),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: <Widget>[
                        Text('主题',
                            style: Theme.of(context).textTheme.titleMedium),
                        const SizedBox(height: 2),
                        Text(
                          '切换立即生效，整个 App 一起变；课表里课程的颜色不变。',
                          style: Theme.of(context)
                              .textTheme
                              .bodySmall
                              ?.copyWith(color: t.inkSoft),
                        ),
                        const SizedBox(height: 12),
                        Wrap(
                          spacing: 10,
                          runSpacing: 10,
                          children: <Widget>[
                            for (final AppThemeId id in AppThemeId.values)
                              _ThemeCard(
                                id: id,
                                selected: controller.themeId == id,
                                onTap: () => controller.select(id),
                              ),
                          ],
                        ),
                      ],
                    ),
                  ),
                  // 玻璃强度：只有 B（深蓝玻璃）有意义。A/C 下置灰并说明原因，
                  // 避免用户以为"选了没反应"。
                  _Row(
                    icon: Icons.blur_on_outlined,
                    title: '玻璃强度',
                    subtitle: controller.themeId == AppThemeId.deepGlass
                        ? '自动会按帧率降级（p90 > 55fps 标准）；课程块永远不模糊'
                        : '当前主题没有玻璃效果（只有「深蓝玻璃」有）',
                    trailing: controller.themeId == AppThemeId.deepGlass
                        ? DropdownButton<GlassMode>(
                            value: controller.glassMode,
                            underline: const SizedBox.shrink(),
                            borderRadius: BorderRadius.circular(t.radiusS),
                            style: Theme.of(context)
                                .textTheme
                                .bodySmall
                                ?.copyWith(color: t.ink),
                            items: <DropdownMenuItem<GlassMode>>[
                              for (final GlassMode mode in GlassMode.values)
                                DropdownMenuItem<GlassMode>(
                                  value: mode,
                                  child: Text(mode.label),
                                ),
                            ],
                            onChanged: (GlassMode? mode) {
                              if (mode != null) {
                                controller.setGlassMode(mode);
                              }
                            },
                          )
                        : Text('—', style: TextStyle(color: t.inkFaint)),
                  ),
                ],
              ),
              // t130：「模型与凭据」组已删除 —— Key 只在「供应商」卡里填；
              // 套餐账号状态/登录只在对话页「模型选择器 → 套餐通道」出现。
              _Group(
                title: '隐私与记忆',
                children: <Widget>[
                  _Row(
                    icon: Icons.cookie_outlined,
                    title: 'Cookie 只留本机',
                    subtitle: '校务会话保存在本地 WebView',
                    trailing: Icon(Icons.verified_outlined, color: t.inkFaint),
                  ),
                  _Row(
                    icon: Icons.storage_outlined,
                    title: '对话记忆',
                    subtitle: '本地 SQLite，可在对话页清空',
                    trailing: Icon(Icons.verified_outlined, color: t.inkFaint),
                  ),
                ],
              ),
              _Group(
                title: '关于',
                children: <Widget>[
                  // t156：当前数据根（覆盖时标注来源）—— 干净环境验证时一眼确认用的是哪份数据。
                  _Row(
                    key: const Key('settings-data-dir'),
                    icon: Icons.folder_outlined,
                    title: '数据目录',
                    subtitle: AppPaths.isOverridden
                        ? '${AppPaths.dataDir}（覆盖：${AppPaths.overrideSource.label}）'
                        : '${AppPaths.dataDir}（默认）',
                  ),
                  if (AppPaths.isOverridden)
                    _Row(
                      key: const Key('settings-portable-mode'),
                      icon: Icons.luggage_outlined,
                      title: '便携/隔离模式',
                      subtitle: '数据在 ${AppPaths.dataDir}',
                    ),
                  _Row(
                    icon: Icons.info_outline,
                    title: '版本',
                    subtitle: '云农之窗 $version · ${BuildInfo.label}',
                    trailing: Text('检查更新', style: TextStyle(color: t.inkSoft)),
                  ),
                ],
              ),
            ],
          ),
        );
      },
    );
  }
}

/// 一张主题预览卡：用**该主题自己的 token** 画，所见即所切。
class _ThemeCard extends StatelessWidget {
  const _ThemeCard(
      {required this.id, required this.selected, required this.onTap});

  final AppThemeId id;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final YunongTokens current = context.tokens;
    final Brightness brightness = Theme.of(context).brightness;
    final YunongTokens preview = tokensFor(id, brightness);
    final bool isCurrent = id == AppThemeId.values[0];
    return Semantics(
      button: true,
      selected: selected,
      label: '切换到${id.label}主题',
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(current.radiusM),
        child: AnimatedContainer(
          duration: current.fast,
          curve: current.ease,
          width: 176,
          padding: const EdgeInsets.all(10),
          decoration: BoxDecoration(
            color: preview.canvas,
            borderRadius: BorderRadius.circular(current.radiusM),
            border: Border.all(
              color: selected ? current.focusRing : current.hairline,
              width: selected ? 2 : 1,
            ),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: <Widget>[
              Container(
                height: 44,
                decoration: BoxDecoration(
                  color: preview.surface,
                  borderRadius: BorderRadius.circular(preview.radiusS),
                  border: Border.all(color: preview.hairline),
                ),
                padding: const EdgeInsets.all(6),
                child: Row(
                  children: <Widget>[
                    Container(
                      width: 4,
                      height: 24,
                      decoration: BoxDecoration(
                        color: preview.accent,
                        borderRadius: BorderRadius.circular(2),
                      ),
                    ),
                    const SizedBox(width: 6),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        mainAxisAlignment: MainAxisAlignment.center,
                        children: <Widget>[
                          Container(
                            height: 5,
                            width: 54,
                            decoration: BoxDecoration(
                              color: preview.ink.withValues(alpha: 0.85),
                              borderRadius: BorderRadius.circular(2),
                            ),
                          ),
                          const SizedBox(height: 4),
                          Container(
                            height: 5,
                            width: 34,
                            decoration: BoxDecoration(
                              color: preview.inkSoft,
                              borderRadius: BorderRadius.circular(2),
                            ),
                          ),
                        ],
                      ),
                    ),
                  ],
                ),
              ),
              const SizedBox(height: 8),
              Text(
                id.label,
                style: TextStyle(
                  fontSize: 13,
                  fontWeight: current.strongWeight,
                  color: preview.ink,
                ),
              ),
              const SizedBox(height: 1),
              Text(
                selected ? '当前使用' : id.hint,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 11, color: preview.inkSoft),
              ),
              if (isCurrent) const SizedBox.shrink(),
            ],
          ),
        ),
      ),
    );
  }
}

class _Group extends StatelessWidget {
  const _Group({required this.title, required this.children});

  final String title;
  final List<Widget> children;

  @override
  Widget build(BuildContext context) {
    final YunongTokens t = context.tokens;
    return Padding(
      padding: const EdgeInsets.only(bottom: 14),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: <Widget>[
          Padding(
            padding: const EdgeInsets.fromLTRB(4, 6, 4, 6),
            child: Text(
              title,
              style: Theme.of(context)
                  .textTheme
                  .labelSmall
                  ?.copyWith(color: t.inkSoft),
            ),
          ),
          Container(
            // 环形阴影代替边框（不占布局、随圆角走）
            decoration: BoxDecoration(
              color: t.surface,
              borderRadius: BorderRadius.circular(t.radiusL),
              boxShadow: <BoxShadow>[...t.shadowPanel, ...ringShadow(t.hairline)],
            ),
            child: Column(
              children: <Widget>[
                for (int i = 0; i < children.length; i++) ...<Widget>[
                  if (i > 0)
                    Divider(
                        height: t.borderWidth,
                        thickness: t.borderWidth,
                        color: t.hairline),
                  children[i],
                ],
              ],
            ),
          ),
        ],
      ),
    );
  }
}

class _Row extends StatelessWidget {
  const _Row(
      {super.key,
      required this.icon,
      required this.title,
      this.subtitle,
      this.trailing});
  final Widget? trailing;

  final IconData icon;
  final String title;
  final String? subtitle;

  @override
  Widget build(BuildContext context) {
    final YunongTokens t = context.tokens;
    return Padding(
      padding: const EdgeInsets.fromLTRB(14, 12, 14, 12),
      child: Row(
        children: <Widget>[
          Icon(icon, size: 18, color: t.inkSoft),
          const SizedBox(width: 12),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: <Widget>[
                Text(title, style: Theme.of(context).textTheme.bodyLarge),
                if (subtitle != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 2),
                    child: Text(
                      subtitle!,
                      style: Theme.of(context)
                          .textTheme
                          .bodySmall
                          ?.copyWith(color: t.inkSoft),
                    ),
                  ),
              ],
            ),
          ),
          if (trailing != null) ...<Widget>[
            const SizedBox(width: 12),
            trailing!
          ],
        ],
      ),
    );
  }
}

/// t86：从设置页打开对话设置（与对话页共享同一个 controller，保存即同步）。
Future<void> openChatSettingsDialog(
  BuildContext context,
  ChatSettingsController controller, {
  AuthService? auth,
  SecretService? secrets,
  Future<String?> Function(String apiKey)? keyValidator,
  void Function(SecretStatus status)? onKeySaved,
}) async {
  final ChatSettings? result = await showDialog<ChatSettings>(
    context: context,
    builder: (BuildContext inner) => ChatSettingsSection(
      settings: controller.settings,
      searchLabel: '（在对话页查看）',
      searchHint: '',
      // t88：账号与 Key 能力已下沉到面板，设置页只需注入服务
      accountLabel: auth == null ? '未登录' : (auth.signedIn ? auth.maskedEmail : '未登录'),
      signedIn: auth?.signedIn ?? false,
      onSignIn: auth == null ? null : () => signInViaAuth(inner, auth),
      onSignOut: auth == null ? null : auth.signOut,
      secrets: secrets,
      keyValidator: keyValidator,
      onKeySaved: onKeySaved,
    ),
  );
  if (result != null) {
    controller.apply(result);
  }
}

/// 套餐登录（设置页内）：打开登录对话框 → 调 AuthService。
Future<void> signInViaAuth(BuildContext context, AuthService auth) async {
  final ({String email, String password})? result =
      await showDialog<({String email, String password})>(
    context: context,
    builder: (BuildContext context) => const CloudLoginDialog(),
  );
  if (result == null) {
    return;
  }
  try {
    await auth.signIn(email: result.email, password: result.password);
  } on AuthException catch (failure) {
    // 与对话页一致：中文文案，不显示原始错误串
    if (context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(failure.userHint)));
    }
  }
}
