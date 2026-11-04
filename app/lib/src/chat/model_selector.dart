// 对话页模型与 effort 选择条（t115）。
//
// 为什么在对话框里而不是设置页：
// owner 的 IA 是「设置只选供应商；模型/推理强度属于“这次对话怎么问”」。
// 没有 provider 信息时也能用：只列出内置模型。

import 'package:flutter/material.dart';

class ModelSelector extends StatefulWidget {
  const ModelSelector({super.key, this.models = const <String>[], this.initialModel = 'deepseek-chat'});

  /// provider 自定义模型（空 = 只用内置）。
  final List<String> models;
  final String initialModel;

  @override
  State<ModelSelector> createState() => ModelSelectorState();
}

class ModelSelectorState extends State<ModelSelector> {
  static const List<String> _builtinModels = <String>['deepseek-chat', 'deepseek-reasoner'];

  late String _model = widget.initialModel;

  /// 当前选择（供测试与发送时读取）。
  String get model => _model;

  String _effort = 'auto';

  String get effort => _effort;

  @override
  Widget build(BuildContext context) {
    final ThemeData theme = Theme.of(context);
    final List<String> models = <String>{
      ...widget.models,
      ..._builtinModels,
    }.toList();
    if (!models.contains(_model)) {
      _model = models.first;
    }
    return Padding(
      padding: const EdgeInsets.only(bottom: 2),
      child: Wrap(
        spacing: 8,
        runSpacing: 2,
        crossAxisAlignment: WrapCrossAlignment.center,
        children: <Widget>[
          DropdownButton<String>(
            key: const Key('chat-model-selector'),
            value: _model,
            isDense: true,
            underline: const SizedBox.shrink(),
            style: theme.textTheme.bodySmall,
            items: <DropdownMenuItem<String>>[
              for (final String item in models)
                DropdownMenuItem<String>(value: item, child: Text(item)),
            ],
            onChanged: (String? value) {
              if (value != null) {
                setState(() => _model = value);
              }
            },
          ),
          DropdownButton<String>(
            key: const Key('chat-effort-selector'),
            value: _effort,
            isDense: true,
            underline: const SizedBox.shrink(),
            style: theme.textTheme.bodySmall,
            items: const <DropdownMenuItem<String>>[
              DropdownMenuItem<String>(value: 'auto', child: Text('effort 自动')),
              DropdownMenuItem<String>(value: 'low', child: Text('effort 低')),
              DropdownMenuItem<String>(value: 'medium', child: Text('effort 中')),
              DropdownMenuItem<String>(value: 'high', child: Text('effort 高')),
            ],
            onChanged: (String? value) {
              if (value != null) {
                setState(() => _effort = value);
              }
            },
          ),
        ],
      ),
    );
  }
}
