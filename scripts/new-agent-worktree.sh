#!/usr/bin/env bash
# 为子代理创建独立工作树，避免并发写入冲突（Linux / macOS / CI 用）。
# 用法：bash scripts/new-agent-worktree.sh <name> [base]
set -euo pipefail

name="${1:?用法: new-agent-worktree.sh <name> [base]}"
base="${2:-main}"

if [[ ! "$name" =~ ^[a-z0-9][a-z0-9-]*$ ]]; then
  echo "名字只能包含小写字母、数字与连字符：$name" >&2
  exit 1
fi

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
path="$repo/.worktrees/$name"
branch="agent/$name"

if [[ ! -d "$repo/.git" ]]; then
  echo "当前目录不是 git 仓库：$repo" >&2
  exit 1
fi

if [[ -e "$path" ]]; then
  echo "工作树已存在：$path" >&2
  exit 1
fi

mkdir -p "$repo/.worktrees"

if git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; then
  git -C "$repo" worktree add "$path" "$branch"
else
  git -C "$repo" worktree add -b "$branch" "$path" "$base"
fi

echo
echo "已创建：$path"
echo "分支：  $branch（基于 $base）"
echo
echo "派活时请把这段写进子代理提示词："
echo "  你的工作目录是 $path（git worktree，分支 $branch）。"
echo "  只允许修改该目录下的文件；不要切换或写入其他工作树。"
echo "  完成后在该目录内 git add -A && git commit，并回报分支名 / commit hash / 改动清单。"
echo
