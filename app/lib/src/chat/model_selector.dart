// 对话页模型与 effort 选择条（t115；t119 改为可收缩布局）。
//
// 为什么在对话框里：owner 的 IA 是「设置只选供应商；模型/推理强度属于“这次怎么问”」。
//
// t119 教训：**不要用 Wrap 包 DropdownButton** —— DropdownButton 的固有宽度
// （模型名 + 箭头）在 480/320 窄窗会让内部横向 Row 溢出（实测 36px）。
// 现在用 `Row + Flexible + isExpanded`，两个下拉都能压缩并省略文本。

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
      child: Row(
        children: <Widget>[
          Flexible(
            child: DropdownButton<String>(
              key: const Key('chat-model-selector'),
              value: _model,
              isExpanded: true,
              isDense: true,
              underline: const SizedBox.shrink(),
              style: theme.textTheme.bodySmall,
              items: <DropdownMenuItem<String>>[
                for (final String item in models)
                  DropdownMenuItem<String>(
                    value: item,
                    child: Text(item, maxLines: 1, overflow: TextOverflow.ellipsis),
                  ),
              ],
              onChanged: (String? value) {
                if (value != null) {
                  setState(() => _model = value);
                }
              },
            ),
          ),
          const SizedBox(width: 8),
          Flexible(
            child: DropdownButton<String>(
              key: const Key('chat-effort-selector'),
              value: _effort,
              isExpanded: true,
              isDense: true,
              underline: const SizedBox.shrink(),
              style: theme.textTheme.bodySmall,
              items: const <DropdownMenuItem<String>>[
                DropdownMenuItem<String>(
                  value: 'auto',
                  child: Text('effort 自动', maxLines: 1, overflow: TextOverflow.ellipsis),
                ),
                DropdownMenuItem<String>(
                  value: 'low',
                  child: Text('effort 低', maxLines: 1, overflow: TextOverflow.ellipsis),
                ),
                DropdownMenuItem<String>(
                  value: 'medium',
                  child: Text('effort 中', maxLines: 1, overflow: TextOverflow.ellipsis),
                ),
                DropdownMenuItem<String>(
                  value: 'high',
                  child: Text('effort 高', maxLines: 1, overflow: TextOverflow.ellipsis),
                ),
              ],
              onChanged: (String? value) {
                if (value != null) {
                  setState(() => _effort = value);
                }
              },
            ),
          ),
        ],
      ),
    );
  }
}
