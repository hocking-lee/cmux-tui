# cmux-tui

CMUX (3GPP TS 27.010) 串口调试工具。单文件静态二进制，无运行时依赖。

在一条物理串口上建立复用链路，自由选择子通道收发数据，同时观察最多两个子通道，
每条记录带毫秒时间戳与 hex/ASCII 双列显示。

```
┌ CMUX Debugger ─────────────────────────────────────────────────────────┐
│ /dev/ttyUSB0 115200 8N1 │ ● MUX UP │ rx 1204 tx 3 │ fcs_err 0          │
├─ [F1] DLCI 1  UP ──────────────────┬─ [F2] DLCI 3  UP ◀ ──────────────┤
│ 19:04:12.331 TX 8B                 │ 19:04:13.002 RX 4B               │
│ 41 54 2B 43 47 53 4E 0D  AT+CGSN.  │ 4F 4B 0D 0A              OK..    │
│ 19:04:12.402 RX 12B                │                                  │
│ 0D 0A 38 36 31 32 33 34  ..861234  │                                  │
├────────────────────────────────────┴─────────────────────────────────┤
│ ASCII  AT+CGSN▌                                       → DLCI 1        │
└──────────────────────────────────────────────────────────────────────┘
```

## 安装

Arch Linux：

```bash
cd packaging/arch && makepkg -si
```

其他发行版可以直接用静态二进制（零依赖，拷过去就能跑）：

```bash
./scripts/build-static.sh
# 产物：target/{x86_64,aarch64}-unknown-linux-musl/release/cmux-tui
```

## 用法

```bash
cmux-tui --list-ports                                  # 列出可用串口
cmux-tui --port /dev/ttyUSB0 --baud 115200 --dlci 1,3  # 打开并自动建链
cmux-tui --port /dev/ttyUSB0 --no-at                   # 对端已在复用模式
cmux-tui --port /dev/ttyUSB0 --log session.log         # 同时落盘
```

默认先做 AT 握手（`AT` → `OK` → `AT+CMUX=0,0,5,127,10,3,30,10,2`），再对指定的
DLCI 发 SABM 建链。等待 UA 超时 3 秒、重试 3 次；收到 DM 表示对端拒绝，立即标记失败。

## 按键

| 键 | 动作 |
|---|---|
| `F1` / `F2` / `Tab` | 选择发送目标通道（标题栏 `◀` 标记） |
| `Enter` | 发送 |
| `↑` / `↓` | 输入历史 |
| `Ctrl+T` | ASCII ⇄ HEX 输入模式 |
| `Ctrl+O` | 绑定 DLCI(1–63) 到当前槽位并建链 |
| `Ctrl+K` | 控制通道命令（MSC / DISC / CLD） |
| `Ctrl+R` | 原始帧调试视图 |
| `PgUp` / `PgDn` / `End` | 滚动 / 回到跟随最新 |
| `Ctrl+L` / `Ctrl+S` / `Ctrl+Q` | 清当前通道 / 日志开关 / 退出 |

模式切换用 `Ctrl+T` 而不是更顺手的 `Ctrl+H`：终端里 `Ctrl+H` 发送的是 `0x08`，
会被解析成退格键，两者冲突。

ASCII 模式发送时默认追加 `\r`（`--no-cr` 关闭）；HEX 模式下不追加任何字节，
输入 `41 54 0D` 或 `41540D` 均可。

## 排查建链问题

`Ctrl+R` 打开原始帧视图，逐帧列出物理串口上的内容：时间戳、方向、DLCI、帧类型
（SABM/UA/DM/DISC/UIH）、P/F 位、长度、FCS 是否通过，以及无法成帧的垃圾字节。
建链失败时先看这里——多数问题是对端根本没进复用模式，或波特率不匹配导致 FCS 全错。

## 构建

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
./scripts/build-static.sh
```

脚本构建两个架构并断言产物零动态依赖（`readelf` 无 `NEEDED`、无 `INTERP`）。
aarch64 交叉编译用 rustc 自带的 `rust-lld`，不需要 zig、cross 或交叉 gcc。

判断静态链接不要看 `ldd` 的输出文本：x86_64 musl 产物是 static-pie，`ldd` 打印
`statically linked` 而非 `not a dynamic executable`，按文本匹配会误判。

## 范围

仅支持 Basic 模式帧格式，工具始终作为 DTE 侧发起方。不支持 Advanced 模式
（`0x7E` 标志 + 转义），不把子通道暴露成 PTY，不支持 Windows / macOS。

串口断开后不做进程内热重连——真正的重连要求把串口句柄的所有权从读写线程收回，
会显著复杂化线程模型。断开时界面会明确提示重启工具。

## 开发

```bash
cargo test                              # 175 个测试，全部不需要真实硬件
cargo clippy --all-targets -- -D warnings
```

串口被抽象成 `ByteStream` trait，测试用内存实现 `MockStream` 驱动，因此帧编解码、
AT 握手、会话状态机、界面渲染全都能脱离硬件验证。`tests/end_to_end.rs` 里的回环
测试由 `MockStream` 扮演对端模块，跑完「握手 → 双通道建链 → 双向收发 → 拆链」
整条链路。
