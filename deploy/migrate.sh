#!/bin/sh
# 幂等迁移执行器：按文件名顺序应用 crates/server/migrations/*.sql，只应用没记录过的。
#
# 为什么需要它：`docker compose up` 在全新环境里数据库是空的，而服务端启动时会
# 自检 usage_samples 等表结构，缺表就拒绝启动（这是有意的，避免"带病上线"）。
# 但原来的 compose 没有任何东西跑迁移 → 全新部署永远起不来。
#
# 用一张 schema_migrations 表记录已应用的脚本，保证：
# - 重复 `docker compose up` 不会重复执行 0001/0002（它们不是幂等的）；
# - 新增 000N 会被自动应用；
# - 任何一条失败立即退出（ON_ERROR_STOP），不会留下"半个 schema"却继续启动服务。
set -eu

: "${PGHOST:=db}"
: "${PGUSER:=yunong}"
: "${PGDATABASE:=yunong}"
export PGHOST PGUSER PGDATABASE

MIGRATIONS_DIR="${MIGRATIONS_DIR:-/migrations}"

echo "== 等待数据库可连接 =="
i=0
while ! psql -tAc "SELECT 1" >/dev/null 2>&1; do
    i=$((i + 1))
    if [ "$i" -gt 60 ]; then
        echo "数据库 60 秒内仍不可连接" >&2
        exit 1
    fi
    sleep 1
done

psql -v ON_ERROR_STOP=1 -c "
CREATE TABLE IF NOT EXISTS schema_migrations (
    filename   text PRIMARY KEY,
    applied_at timestamptz NOT NULL DEFAULT now()
);" >/dev/null

applied=0
skipped=0
for file in $(ls "$MIGRATIONS_DIR"/*.sql | sort); do
    name=$(basename "$file")
    exists=$(psql -tAc "SELECT 1 FROM schema_migrations WHERE filename = '$name'")
    if [ "$exists" = "1" ]; then
        echo "== 跳过（已应用）$name"
        skipped=$((skipped + 1))
        continue
    fi
    echo "== 应用 $name"
    psql -v ON_ERROR_STOP=1 -f "$file"
    psql -v ON_ERROR_STOP=1 -c "INSERT INTO schema_migrations (filename) VALUES ('$name')" >/dev/null
    applied=$((applied + 1))
done

echo "== 完成：应用 $applied 个，跳过 $skipped 个"
