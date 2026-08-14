#!/usr/bin/env bash
# 构建两个架构的静态二进制，并硬断言产物零动态依赖。
set -euo pipefail

TARGETS=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)

for t in "${TARGETS[@]}"; do
    echo "==> building $t"
    cargo build --release --target "$t"
    bin="target/$t/release/cmux-tui"

    needed=$(readelf -d "$bin" 2>/dev/null | grep -c NEEDED || true)
    interp=$(readelf -l "$bin" 2>/dev/null | grep -c INTERP || true)

    if [ "$needed" -ne 0 ] || [ "$interp" -ne 0 ]; then
        echo "FAIL: $bin 不是全静态 (NEEDED=$needed INTERP=$interp)" >&2
        readelf -d "$bin" | grep NEEDED >&2 || true
        exit 1
    fi
    echo "OK: $bin 全静态 ($(du -h "$bin" | cut -f1))"
done
echo "全部通过"
