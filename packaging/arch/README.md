# Arch Linux 打包

`PKGBUILD` 从 GitHub 上的 `v$pkgver` tag 拉取源码构建，产物是链接系统 glibc 的
动态二进制（Arch 打包惯例），依赖只有 `gcc-libs`。

## 本地构建并安装

```bash
cd packaging/arch
makepkg -si
```

`makepkg` 会依次跑 `prepare`（拉依赖）、`build`、`check`（跑全部 175 个测试）、
`package`。测试全部用内存流驱动，不需要接真实串口硬件，所以在打包机上也能跑。

产物内容：

```
usr/bin/cmux-tui
usr/share/doc/cmux-tui/README.md
usr/share/licenses/cmux-tui/LICENSE
```

## 发布新版本

1. 改 `Cargo.toml` 的 `version`。
2. 提交后打 tag：`git tag -a v0.0.2 -m "cmux-tui 0.0.2" && git push --tags`。
3. 改本目录 `PKGBUILD` 的 `pkgver`，把 `pkgrel` 重置为 `1`。
4. 重新生成校验信息：`makepkg --printsrcinfo > .SRCINFO`。
5. 只改打包脚本而源码未变时，不动 `pkgver`，把 `pkgrel` 加一。

## 发布到 AUR

```bash
git clone ssh://aur@aur.archlinux.org/cmux-tui.git aur-cmux-tui
cp PKGBUILD .SRCINFO aur-cmux-tui/
cd aur-cmux-tui && git add -A && git commit -m "Initial import: cmux-tui 0.0.1" && git push
```

AUR 要求 `.SRCINFO` 与 `PKGBUILD` 保持同步，改完 `PKGBUILD` 一定要重新生成。

## 两个已知点

**`makedepends=('cargo')`** 由官方 `rust` 包满足（它 provides `cargo`）。如果你的
工具链是 rustup 装的，pacman 数据库里没有对应记录，`makepkg` 会报缺依赖——这种情况
用 `makepkg -si --nodeps`，构建本身不受影响。

**`options=('!debug')`** 是必要的：`Cargo.toml` 的 release profile 已经设了
`strip = true`，再让 makepkg 抽调试符号只会产出一个空的 `-debug` 包，并在打包时
报 `No debugging symbols`。

## 静态二进制

pacman 包走系统 glibc。如果需要零依赖的静态单文件（拷到任意 Linux 直接跑，
包括无包管理的嵌入式 rootfs），用仓库根目录的脚本单独出：

```bash
./scripts/build-static.sh
```

它构建 `x86_64-unknown-linux-musl` 与 `aarch64-unknown-linux-musl` 两个目标，
并硬断言产物零动态依赖。
