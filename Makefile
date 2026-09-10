# Rodeo 开发/构建任务
# 依赖 cargo-leptos（0.3.x）；运行 `make help` 查看全部命令。

CARGO_LEPTOS := cargo leptos

.PHONY: help dev watch serve build build-release test check fmt clippy clean reset-data

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
