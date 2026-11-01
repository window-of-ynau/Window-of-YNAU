#!/usr/bin/env bash
# 服务端镜像冒烟：真起容器 → 探 /healthz、/readyz → 断言二进制不是空壳、容器不是 root。
#
# 为什么存在：本轮踩过"镜像里打包的是空壳二进制"——Dockerfile 的依赖缓存层保留了空壳
# 源码的 mtime，`COPY crates` 之后 cargo 认为没变化、跳过重编，镜像里装了个立刻退出的
# `fn main(){}`（314KB、exit 0、无日志）。CI 与 release 共用这个脚本，保证"构建出来的
# 镜像真的能跑"而不是"构建命令返回 0"。
#
# 用法：
#   scripts/docker-smoke.sh <镜像> [容器名] [宿主端口]
#   KEEP_CONTAINER=1 scripts/docker-smoke.sh ...   # 失败时保留容器排查
#
# 依赖：docker、curl；JSON 断言优先用 python3（runner 自带），没有则退回 python。
set -uo pipefail

image="${1:-}"
name="${2:-yunong-smoke}"
port="${3:-18080}"

if [ -z "$image" ]; then
  echo "用法: $0 <镜像> [容器名] [宿主端口]" >&2
  exit 2
fi

py=""
for cand in python3 python; do
  if command -v "$cand" > /dev/null 2>&1; then
    py="$cand"
    break
  fi
done
if [ -z "$py" ]; then
  echo "::error::找不到 python3/python，无法断言 /healthz 的 JSON 契约" >&2
  exit 2
fi

cleanup() {
  if [ "${KEEP_CONTAINER:-0}" = "1" ]; then
    echo "KEEP_CONTAINER=1：保留容器 ${name}（docker logs ${name} 可看日志）"
    return
  fi
  docker rm -f "$name" > /dev/null 2>&1 || true
}
trap cleanup EXIT

# json_assert <期望的 python 断言体> <JSON>
json_assert() {
  printf '%s' "$2" | "$py" -c "import json,sys; d=json.load(sys.stdin); assert $1, d" > /dev/null
}

echo "== 0) 起新容器前先清掉同名旧容器"
docker rm -f "$name" > /dev/null 2>&1 || true

echo "== 1) 二进制不是空壳（真实产物 10MB 量级，空壳只有几百 KB）"
size=$(docker run --rm --entrypoint /bin/sh "$image" -c 'stat -c%s /usr/local/bin/yunong-server') || size=""
case "$size" in
  '' | *[!0-9]*)
    echo "::error::取不到镜像里的二进制大小（stat 失败，size='$size'）"
    exit 1
    ;;
esac
echo "binary_bytes=$size"
if [ "$size" -lt 2000000 ]; then
  echo "::error::镜像里的 yunong-server 只有 ${size} bytes，疑似空壳二进制"
  exit 1
fi

echo "== 2) 非 root 运行（Dockerfile 约定 uid/gid=10001）"
uid=$(docker run --rm --entrypoint /bin/sh "$image" -c 'id -u') || uid=""
echo "uid=$uid"
if [ "$uid" != "10001" ]; then
  echo "::error::容器以 uid='$uid' 运行，预期 10001"
  exit 1
fi

echo "== 3) 起容器：容器内监听 0.0.0.0:8080（默认 127.0.0.1 从容器外探不到）"
docker run -d --name "$name" \
  -e YUNONG_BIND=0.0.0.0:8080 \
  -p "127.0.0.1:${port}:8080" \
  "$image" > /dev/null
echo "started=$name port=$port"

echo "== 4) 等 /healthz 就绪（最多 30s；容器先死＝空壳行为，直接判红）"
body=""
for i in $(seq 1 30); do
  if body=$(curl -fsS --max-time 2 "http://127.0.0.1:${port}/healthz" 2>/dev/null); then
    echo "healthz_ready_after=${i}s"
    break
  fi
  if [ "$(docker inspect -f '{{.State.Running}}' "$name" 2>/dev/null || echo false)" != "true" ]; then
    echo "::error::容器启动即退出（二进制跑不起来；容器日志见下）"
    docker logs "$name" 2>&1 || true
    exit 1
  fi
  sleep 1
done
if [ -z "$body" ]; then
  echo "::error::30s 内 /healthz 仍未就绪（容器日志见下）"
  docker logs "$name" 2>&1 || true
  exit 1
fi
echo "healthz=$body"

echo "== 5) /healthz 契约：ok=true / status=alive / version 非空"
json_assert "d['ok'] is True" "$body" \
  && json_assert "d['data']['status'] == 'alive'" "$body" \
  && json_assert "isinstance(d['data']['version'], str) and d['data']['version'].strip()" "$body" \
  || { echo "::error::/healthz 返回不符合约定"; exit 1; }

echo "== 6) /readyz 契约：无库模式必须 200 + db=not_configured"
ready=$(curl -fsS --max-time 2 "http://127.0.0.1:${port}/readyz") || ready=""
echo "readyz=$ready"
if [ -z "$ready" ]; then
  echo "::error::/readyz 请求失败（无 DATABASE_URL 时应返回 200 + not_configured）"
  exit 1
fi
json_assert "d['ok'] is True" "$ready" \
  && json_assert "d['data']['db'] == 'not_configured'" "$ready" \
  || { echo "::error::/readyz 在无库模式下不符合预期"; exit 1; }

echo "SMOKE_OK image=${image} binary_bytes=${size} uid=${uid}"
