# RCTool 常用命令（just --list 看全部）。装 just：brew install just
#
# 前端是纯静态文件，日常开发不需要 node；打包用的 tauri-cli 走 npx 临时获取，
# 也不必全局安装。打包流程与 .github/workflows/release.yml 保持一致——改这里
# 时记得同步改那边（BlackHole 的 URL/sha256 两处各有一份）。

set shell := ["bash", "-euo", "pipefail", "-c"]

tauri := "npx --yes @tauri-apps/cli@2"

# 内嵌进 macOS full 版的 BlackHole 官方安装器（GPL-3.0，与本项目同证可合法内嵌）。
# URL/sha256 取自 homebrew-cask 的 blackhole-2ch 定义，升级时同步改 release.yml。
bh_url := "https://existential.audio/downloads/BlackHole2ch-0.7.1.pkg"
bh_sha := "57b540f27a3e29c37e310e01bee0fdfab76733087e47f997ef9dccf851400dcf"
bh_pkg := "apps/tray/src-tauri/resources/BlackHole2ch-0.7.1.pkg"

# 本平台的默认 bundle 目标
bundles := if os() == "macos" { "app,dmg" } else if os() == "windows" { "msi,nsis" } else { "deb,appimage" }

default:
    @just --list

# ---------------------------------------------------------------- 开发

# 起 GUI（前端纯静态，改 src/ 刷新窗口即可）
run:
    cargo run -p rctool-tray

# tauri dev：走 tauri-cli，嵌 Info.dev.plist，前端改动自动重载
dev:
    cd apps/tray && {{ tauri }} dev

# CLI：just cli scan / just cli run --output BlackHole
cli *args:
    cargo run -p rctool-cli --release -- {{ args }}

# ---------------------------------------------------------------- 检查

# 核心逻辑单元测试
test:
    cargo test -p rctool-core

# 全 workspace 编译（含 Tauri 壳与平台专属模块）
build:
    cargo build --workspace

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

# 推之前跑这个：与 CI 同样的三步
ci: test build clippy

# ---------------------------------------------------------------- 打包

# 下载并校验内嵌用的 BlackHole 安装器（不入库，.gitignore 忽略 *.pkg）
blackhole:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -f "{{ bh_pkg }}" ] && echo "{{ bh_sha }}  {{ bh_pkg }}" | shasum -a 256 -c - >/dev/null 2>&1; then
        echo "已有且校验通过：{{ bh_pkg }}"
        exit 0
    fi
    mkdir -p "$(dirname "{{ bh_pkg }}")"
    curl -fL "{{ bh_url }}" -o "{{ bh_pkg }}"
    echo "{{ bh_sha }}  {{ bh_pkg }}" | shasum -a 256 -c -

# 打安装包（默认本平台目标；just bundle dmg 可指定。macOS 上带内嵌安装器即 full 版）
bundle targets=bundles:
    #!/usr/bin/env bash
    set -euo pipefail
    # macOS 签名身份的选取见 `just sign-id`。选不到就 ad-hoc——能跑，但每次
    # 重编后系统权限都要重给，原因写在那条配方里。
    if [ "{{ os() }}" = "macos" ]; then
        id="$(just sign-id)"
        if [ -n "$id" ]; then
            echo "签名身份：$id"
            export APPLE_SIGNING_IDENTITY="$id"
        else
            echo "警告：无可用签名证书，将 ad-hoc 签名——重装后系统权限需要重新授予。" >&2
        fi
    fi
    cd apps/tray && {{ tauri }} build --bundles {{ targets }}

# 解析本机该用哪张证书签名，打印身份名（没有就打印空）
sign-id:
    #!/usr/bin/env bash
    set -euo pipefail
    # 为什么必须固定证书：ad-hoc 签名的 designated requirement 就是
    # `cdhash H"..."`，也就是某一个具体二进制的哈希。重编一次哈希就变，TCC 里
    # 那条授权指向的还是旧哈希，新二进制不满足 DR 就被判无权限——而
    # 「系统设置 > 隐私与安全性」的列表是按 bundle id 显示的，看着还在，实际
    # 已经对不上。用证书签，DR 变成「bundle id + 证书」，与二进制内容无关，
    # 权限就能跨重编保留。
    #
    # 不在仓库里写死具体证书：别人 clone 下来没有你的证书。优先取环境变量；
    # 否则本机恰好只有一张 codesigning 证书时用那张（多张时不猜，避免签错）。
    if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
        echo "$APPLE_SIGNING_IDENTITY"
        exit 0
    fi
    list=$(security find-identity -v -p codesigning 2>/dev/null || true)
    # 形如：  1) ABCD... "Apple Development: Name (TEAMID)"
    count=$(printf '%s\n' "$list" | grep -c '^ *[0-9][0-9]*)' || true)
    if [ "$count" = "1" ]; then
        printf '%s\n' "$list" | sed -n 's/.*"\(.*\)".*/\1/p' | head -1
    else
        echo ""
    fi

# 打包并安装到 /Applications，然后启动（macOS 本机自用）。
install:
    #!/usr/bin/env bash
    set -euo pipefail
    [ "{{ os() }}" = "macos" ] || { echo "just install 目前只支持 macOS" >&2; exit 1; }
    just bundle app
    app="target/release/bundle/macos/RCTool.app"
    pkill -x rctool-tray 2>/dev/null || true
    sleep 1
    rm -rf /Applications/RCTool.app
    # 必须 ditto：cp -R 会破坏签名封印。
    ditto "$app" /Applications/RCTool.app
    codesign --verify --strict /Applications/RCTool.app
    echo "designated requirement:"
    codesign -d -r- /Applications/RCTool.app 2>&1 | tail -1
    open /Applications/RCTool.app

# 打包并部署到远程 Mac，用法：just deploy [host]
deploy host="minits":
    #!/usr/bin/env bash
    set -euo pipefail
    [ "{{ os() }}" = "macos" ] || { echo "just deploy 目前只支持 macOS" >&2; exit 1; }
    just bundle app
    # 必须走 ditto 打包：zip/cp 会丢扩展属性，签名封印随之失效。
    zip=/tmp/RCTool-deploy.zip
    rm -f "$zip"
    ditto -c -k --keepParent target/release/bundle/macos/RCTool.app "$zip"
    scp -o BatchMode=yes "$zip" {{ host }}:/tmp/RCTool-deploy.zip
    ssh -o BatchMode=yes {{ host }} '
        set -e
        pkill -x rctool-tray 2>/dev/null || true
        sleep 1
        rm -rf /Applications/RCTool.app
        ditto -x -k /tmp/RCTool-deploy.zip /Applications/
        rm -f /tmp/RCTool-deploy.zip
        codesign --verify --strict /Applications/RCTool.app
        echo "designated requirement:"
        codesign -d -r- /Applications/RCTool.app 2>&1 | tail -1
        open /Applications/RCTool.app
    '
    rm -f "$zip"
    echo "首次从 ad-hoc 版切过来时，在目标机上跑一次：ssh {{ host }} tccutil reset All dev.rctool.tray"

# 清掉本应用在 TCC 里的全部授权记录（辅助功能/输入监控/蓝牙…）
reset-perms:
    # 只在从 ad-hoc 切到证书签名那一次需要：旧记录绑的是旧 cdhash，留着就是
    # 「列表里有、实际不认」的僵尸条目——手动删条目做的也是这件事。
    tccutil reset All dev.rctool.tray

# CLI 的 release 二进制
bundle-cli:
    cargo build --release -p rctool-cli

# 本机全套产物汇总到 dist/：安装包 + CLI；macOS 额外出 .app.zip 与 lite dmg
dist: bundle-cli
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf dist target/release/bundle && mkdir -p dist

    if [ "{{ os() }}" = "macos" ]; then
        just blackhole                       # full 版要带内嵌安装器
        just bundle app,dmg
        cp target/release/bundle/dmg/*.dmg dist/
        root="$PWD"
        (cd target/release/bundle/macos && for a in *.app; do
            zip -qr "$root/dist/${a%.app}-macos-app.zip" "$a"
        done)
        # lite：移除内嵌安装器后重打一份 dmg，改名区分
        rm -f apps/tray/src-tauri/resources/*.pkg
        rm -rf target/release/bundle/dmg
        just bundle dmg
        for f in target/release/bundle/dmg/*.dmg; do
            cp "$f" "dist/$(basename "${f%.dmg}")-lite.dmg"
        done
    else
        just bundle
        find target/release/bundle -type f \
            \( -name "*.msi" -o -name "*.exe" -o -name "*.deb" -o -name "*.AppImage" \) \
            -exec cp {} dist/ \;
    fi

    case "{{ os() }}" in
        windows) cp target/release/rctool.exe dist/rctool-cli-windows.exe ;;
        *)       cp target/release/rctool "dist/rctool-cli-{{ os() }}" ;;
    esac
    ls -la dist

clean:
    cargo clean
    rm -rf dist
