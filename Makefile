# Rodeo 开发/构建任务
# 依赖 cargo-leptos（0.3.x）；运行 `make help` 查看全部命令。

CARGO_LEPTOS := cargo leptos

# Linux 生产构建的目标平台。Apple Silicon 上保持 linux/amd64，否则会静默产出 arm64 二进制。
LINUX_PLATFORM ?= linux/amd64
# 归档名带上架构，否则 amd64 与 arm64 的分发包同名相撞。
DIST_ARCH := $(subst linux/,,$(LINUX_PLATFORM))

# 容器内 cargo 的 registry 源。crates.io 的 Fastly IPv4 端点在本网络被限速到 ~3KB/s，
# 容器又没有 IPv6 可走，不换镜像就会卡在下载上直到构建失败。置空则用官方 registry：
# 	make package-linux CARGO_REGISTRY_MIRROR=
# 末尾的斜杠是 cargo 对 sparse registry 的硬要求，去掉会报 "must end in a slash"。
CARGO_REGISTRY_MIRROR ?= https://mirrors.ustc.edu.cn/crates.io-index/

# cargo-leptos 还要从 GitHub Releases 取 wasm-bindgen 与 wasm-opt，GitHub 在这类网络下同样是
# ~2KB/s，走宿主上的代理能到 ~1.2MB/s。host.docker.internal 是 Docker Desktop 提供的宿主地址；
# 端口跟着你的本地代理走。置空则直连 GitHub：
# 	make package-linux CARGO_DOWNLOAD_PROXY=
CARGO_DOWNLOAD_PROXY ?= http://host.docker.internal:10900
# 该代理只用于 GitHub 上的工具下载，registry 镜像要绕开它：镜像直连很快，绕代理反而慢一个数量级。
CARGO_NO_PROXY ?= $(shell echo "$(CARGO_REGISTRY_MIRROR)" | cut -d/ -f3),localhost,127.0.0.1

# 宿主侧交叉编译的目标三元组（musl，静态链接）。前置条件只有两个：
# brew install zig、rustup target add x86_64-unknown-linux-musl。
# 注意它不跟随 LINUX_PLATFORM：zig 的目标名、binutils 与头文件路径都按 x86_64 写死在
# scripts/ 里，要出 arm64 得一并改那些，眼下只对齐 Docker 路径的默认架构。
LINUX_TARGET := x86_64-unknown-linux-musl
# 环境变量名用下划线形式，cargo / cc-rs / bindgen 都按这个拼写找。
LINUX_TARGET_ENV := $(subst -,_,$(LINUX_TARGET))
ZIG_LIB_DIR := $(shell zig env 2>/dev/null | sed -n 's/.*\.lib_dir *= *"\([^"]*\)".*/\1/p')

# cc-rs 与 bindgen 都是在依赖包自己的目录（cargo registry）里被调起的，那里的相对路径
# 指向的不是本仓库，所以这三个必须给绝对路径。
export CC_$(LINUX_TARGET_ENV) := $(CURDIR)/scripts/zig-linux-musl-cc
export CXX_$(LINUX_TARGET_ENV) := $(CURDIR)/scripts/zig-linux-musl-cxx
export AR_$(LINUX_TARGET_ENV) := $(CURDIR)/scripts/zig-linux-musl-ar

# bindgen 走的是 libclang，不经过上面那三个包装脚本，得单独喂头文件搜索路径：宿主 clang
# 只带 macOS 的头，解析 Linux 目标时连 stddef.h 都找不到。这里改用 zig 自带的 musl 头；
# -nostdinc 先把 clang 自带的那套（含会 include_next 去找系统头的 stddef.h）排除掉。
export BINDGEN_EXTRA_CLANG_ARGS_$(LINUX_TARGET_ENV) := -nostdinc \
	-isystem $(ZIG_LIB_DIR)/include \
	-isystem $(ZIG_LIB_DIR)/libc/include/x86_64-linux-musl \
	-isystem $(ZIG_LIB_DIR)/libc/include/generic-musl \
	-isystem $(ZIG_LIB_DIR)/libc/include/x86-linux-any \
	-isystem $(ZIG_LIB_DIR)/libc/include/any-linux-any

# cargo-leptos 编译 bin 时会注入这批环境变量。build-linux-host 绕开它直接调 cargo，就得自己
# 补：少了 LEPTOS_OUTPUT_NAME，get_configuration() 会把 output_name 退回 leptos 默认的
# "leptos"，SSR 出来的 hydration 脚本路径与实际 site/pkg 对不上，页面加载不出客户端。
# 取值逐一对应 Cargo.toml 的 [package.metadata.leptos]，改那边记得同步这里。
# 只作用于下面那条 recipe（不做全局 export），免得干扰走 cargo-leptos 的其余目标。
LEPTOS_BIN_ENVS := \
	LEPTOS_OUTPUT_NAME=rodeo \
	LEPTOS_SITE_ROOT=target/site \
	LEPTOS_SITE_PKG_DIR=pkg \
	LEPTOS_SITE_ADDR=127.0.0.1:3000 \
	LEPTOS_RELOAD_PORT=3001 \
	LEPTOS_HASH_FILES=true \
	LEPTOS_ENV=DEV

.PHONY: help dev watch serve build build-release build-linux build-linux-host package-linux test check fmt clippy clean reset-data

help: ## 显示帮助
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

dev: ## 启动开发服务器（文件变更自动热重载）
	$(CARGO_LEPTOS) watch

watch: dev ## 同 dev

serve: ## 以 hydrate 模式启动服务（不热重载）
	$(CARGO_LEPTOS) serve

build: ## 构建（server ssr + client wasm hydrate）
	$(CARGO_LEPTOS) build

build-release: ## 构建 release（优化）
	$(CARGO_LEPTOS) build --release

# 产物落在 dist/：可执行文件 rodeo + 站点目录 site/。运行时把 LEPTOS_SITE_ROOT 指到 site。
# 先清 dist/：--output type=local 是往目标目录里合并而非清空，而 hash-files 让每次构建的
# site/pkg/*.wasm 换名，不清的话上一轮的旧产物会一直攒在包里。
build-linux: ## 构建 Linux x86_64 生产产物到 dist/（Docker 容器内编译）
	rm -rf dist
	docker build \
		--platform $(LINUX_PLATFORM) \
		--target dist \
		--output type=local,dest=dist \
		--build-arg CARGO_REGISTRY_MIRROR=$(CARGO_REGISTRY_MIRROR) \
		--build-arg CARGO_DOWNLOAD_PROXY=$(CARGO_DOWNLOAD_PROXY) \
		--build-arg CARGO_NO_PROXY=$(CARGO_NO_PROXY) \
		-f Dockerfile.build \
		.

# 同样产出 Linux x86_64 二进制，但走宿主的 zig 交叉编译而非 Docker：快得多（复用本地
# target/ 增量），代价是只出可执行文件——site/（含 hydrate 用的 wasm）不在这条路径上，
# 需要 `cargo leptos build` 或 build-linux 才能拿到。产物落在 target/ 与 dist/rodeo。
build-linux-host: ## 交叉编译 Linux x86_64 静态可执行文件到 dist/rodeo（宿主 zig，不用 Docker）
	@command -v zig >/dev/null || { echo "缺少 zig：brew install zig"; exit 1; }
	@rustup target list --installed | grep -qx $(LINUX_TARGET) \
		|| { echo "缺少目标：rustup target add $(LINUX_TARGET)"; exit 1; }
	$(LEPTOS_BIN_ENVS) cargo build --release --target $(LINUX_TARGET)
	mkdir -p dist
	cp target/$(LINUX_TARGET)/release/rodeo dist/rodeo
	@file dist/rodeo
	@ls -lh dist/rodeo

# 非容器部署用：把 dist/ 打成一个自带启动脚本的 tar.gz，scp 到生产机解压即跑。
# start.sh 设 LEPTOS_SITE_ROOT 与 LEPTOS_HASH_FILES，不设 LEPTOS_SITE_ADDR——后者优先级
# 高于 config.toml 的 [server]，一旦在这里导出就会把生产机的配置盖掉，正是要避免的那种
# 静默失效。
#
# 前两个都是**运行期**才被读的：hash-files 打开后 site/pkg 里全是 rodeo.<hash>.js/wasm/css，
# 而 Leptos 生成的 HTML 只有在同样打开 hash-files、且能在二进制同级目录找到 hash.txt 时
# 才会拼上那串哈希；否则它按裸名 /pkg/rodeo.js 发请求，那些文件并不存在，前端资源一律 404。
# 两者缺一不可，所以 hash.txt 必须跟 rodeo 一起进包。
package-linux: build-linux ## 打包非容器部署产物到 dist/rodeo-<arch>.tar.gz
	rm -rf dist/rodeo-$(DIST_ARCH)
	mkdir -p dist/rodeo-$(DIST_ARCH)
	cp dist/rodeo dist/rodeo-$(DIST_ARCH)/rodeo
	cp dist/hash.txt dist/rodeo-$(DIST_ARCH)/hash.txt
	cp -r dist/site dist/rodeo-$(DIST_ARCH)/site
	cp config.example.toml dist/rodeo-$(DIST_ARCH)/config.example.toml
	printf '%s\n' \
		'#!/bin/sh' \
		'set -e' \
		'cd "$$(dirname "$$0")"' \
		'export LEPTOS_SITE_ROOT=site' \
		'export LEPTOS_HASH_FILES=true' \
		'exec ./rodeo config.toml' \
		> dist/rodeo-$(DIST_ARCH)/start.sh
	chmod +x dist/rodeo-$(DIST_ARCH)/start.sh
	tar -czf dist/rodeo-$(DIST_ARCH).tar.gz -C dist rodeo-$(DIST_ARCH)
	@ls -lh dist/rodeo-$(DIST_ARCH).tar.gz
	@shasum -a 256 dist/rodeo-$(DIST_ARCH).tar.gz 2>/dev/null \
		|| sha256sum dist/rodeo-$(DIST_ARCH).tar.gz

test: ## 运行测试（ssr 目标）
	cargo test

check: ## 类型检查（native + wasm）
	cargo check
	cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown

fmt: ## 格式化代码
	cargo fmt

clippy: ## 运行 clippy
	cargo clippy --all-targets --all-features

clean: ## 清理全部构建产物（含 target/site）
	cargo clean

reset-data: ## 删除本地开发数据（bincode 结构变更导致反序列化失败时）
	rm -rf data
