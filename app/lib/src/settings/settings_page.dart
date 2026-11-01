// 设置页（批次 1）：外观是主角——三张主题预览卡 + 明暗模式；其余分组先占位。
//
// 交互约定：
// - 点预览卡 = 立即换主题（不重启、不弹确认）；
// - 「玻璃强度」在批次 2 接（B 的真模糊与降级做完再放出来），现在不显示半成品；
// - 所有颜色/圆角/字号都来自 `context.tokens`，页面本身不写死任何视觉值。

import 'package:flutter/material.dart';

import '../theme/theme_controller.dart';
import '../theme/themes.dart';
import '../theme/yunong_tokens.dart';

class SettingsPage extends StatelessWidget {
  const SettingsPage({super.key, required this.controller, this.version = '0.9.0（W2b）'});

  final ThemeController controller;
  final String version;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: controller,
      builder: (BuildContext context, Widget? _) {
        final YunongTokens t = context.tokens;
        return Scaffold(
          appBar: AppBar(title: const Text('设置')),
          body: ListView(
            padding: const EdgeInsets.fromLTRB(16, 8, 16, 32),
            children: <Widget>[
              _Group(
                title: '外观',
                children: <Widget>[
                  Padding(
                    padding: const EdgeInsets.fromLTRB(14, 12, 14, 14),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: <Widget>[
                        Text('主题', style: Theme.of(context).textTheme.titleMedium),
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
                  _Row(
                    icon: Icons.brightness_6_outlined,
                    title: '明暗',
                    subtitle: '跟随系统时，主题会各自使用浅色/深色两套 token',
                    trailing: SegmentedButton<ThemeMode>(
                      showSelectedIcon: false,
                      // 用 token 着色：A（黑白）下选中段也是黑底白字，不引入 Material 的蓝色
                      style: ButtonStyle(
                        visualDensity: VisualDensity.compact,
                        textStyle: WidgetStatePropertyAll<TextStyle>(
                          Theme.of(context).textTheme.bodySmall ?? const TextStyle(),
                        ),
                        foregroundColor: WidgetStateProperty.resolveWith((Set<WidgetState> s) =>
                            s.contains(WidgetState.selected) ? t.onAccent : t.inkSoft),
                        backgroundColor: WidgetStateProperty.resolveWith((Set<WidgetState> s) =>
                            s.contains(WidgetState.selected) ? t.accent : t.surface),
                        side: WidgetStatePropertyAll<BorderSide>(
                          BorderSide(color: t.controlBorder, width: t.borderWidth),
                        ),
                      ),
                      segments: const <ButtonSegment<ThemeMode>>[
                        ButtonSegment<ThemeMode>(value: ThemeMode.system, label: Text('跟随系统')),
                        ButtonSegment<ThemeMode>(value: ThemeMode.light, label: Text('浅色')),
                        ButtonSegment<ThemeMode>(value: ThemeMode.dark, label: Text('深色')),
                      ],
                      selected: <ThemeMode>{controller.themeMode},
                      onSelectionChanged: (Set<ThemeMode> value) =>
                          controller.setThemeMode(value.first),
                    ),
                  ),
                ],
              ),
              _Group(
                title: '模型与凭据',
                children: <Widget>[
                  _Row(
                    icon: Icons.key_outlined,
                    title: 'BYOK 密钥 / 套餐',
                    subtitle: '在对话页右上角的设置里配置；密钥只存本机安全存储',
                    trailing: Icon(Icons.chevron_right, color: t.inkFaint),
                  ),
                ],
              ),
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
                  _Row(
                    icon: Icons.info_outline,
                    title: '版本',
                    subtitle: '云农之窗 $version',
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
  const _ThemeCard({required this.id, required this.selected, required this.onTap});

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
              style: Theme.of(context).textTheme.labelSmall?.copyWith(color: t.inkSoft),
            ),
          ),
          Container(
            decoration: BoxDecoration(
              color: t.surface,
              borderRadius: BorderRadius.circular(t.radiusL),
              border: Border.all(color: t.hairline, width: t.borderWidth),
              boxShadow: t.shadowPanel,
            ),
            child: Column(
              children: <Widget>[
                for (int i = 0; i < children.length; i++) ...<Widget>[
                  if (i > 0)
                    Divider(height: t.borderWidth, thickness: t.borderWidth, color: t.hairline),
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
  const _Row({required this.icon, required this.title, this.subtitle, this.trailing});

  final IconData icon;
  final String title;
  final String? subtitle;
  final Widget? trailing;

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
          if (trailing != null) ...<Widget>[const SizedBox(width: 12), trailing!],
        ],
      ),
    );
  }
}
