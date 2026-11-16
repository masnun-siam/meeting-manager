# One command per job. The meeting crate, the extension and the desktop UI run in the hook image
# (docker/hooks.Dockerfile); the Tauri crate and the running app need macOS and run on the host.
SHELL := /bin/bash
export BASE_REF ?= main
export PATH := $(HOME)/.cargo/bin:$(PATH)
DOCKER := scripts/hooks/docker.sh
# AGENT=<name> gives a parallel session its own Vite port; unset keeps Tauri's default 1420.
AGENT ?=
PORT := $(if $(AGENT),$(shell printf '%s' '$(AGENT)' | cksum | awk '{print 1421 + $$1 % 500}'),1420)

.PHONY: help setup up all extension desktop doctor check lint fmt test clean-docker

help: ## List the targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  make %-13s %s\n", $$1, $$2}'

setup: ## Once per clone: host tools, hook image, git hooks
	scripts/hooks/install-host-tools.sh
	$(DOCKER) true
	pre-commit install

up: extension desktop ## Build the extension (Docker), then run the desktop app (host, tauri dev)
all: up ## Single-repo product: the same as make up
ifneq ($(with),)
	@echo "with=$(with): this product has one repo; nothing else to start"
endif

extension: ## Build the unpacked extension into extension/dist (load it in chrome://extensions)
	$(DOCKER) sh -c 'cd extension && npx vite build'

desktop: ## Run the desktop app against a Vite dev server on PORT (AGENT-scoped)
	cd desktop && { [ -x node_modules/.bin/tauri ] || npm ci --no-audit --no-fund; }
	cd desktop && npx tauri dev --config '{"build":{"devUrl":"http://localhost:$(PORT)","beforeDevCommand":"npm run dev -- --port $(PORT)"},"app":{"security":{"devCsp":"default-src '\''self'\''; connect-src ipc: http://ipc.localhost ws://localhost:$(PORT); object-src '\''none'\''; base-uri '\''none'\''; frame-ancestors '\''none'\''"}}}'

doctor: ## Health check of the installed tool (config, Keychain, ffmpeg, claude, Chrome host)
	cargo run -q -p meeting -- doctor

check: ## Every gate on every file: full suites, coverage, lint (what CI runs)
	scripts/hooks/run.sh all

lint: ## Format, clippy, types and biome, as CI runs them
	$(DOCKER) sh -c 'cargo fmt --all --check && cargo clippy -p meeting --all-targets --locked -- -D warnings && cd extension && npx tsc --noEmit && npx biome ci . && cd ../desktop && npx tsc --noEmit && npx biome ci .'
	cargo clippy -p meeting-desktop --all-targets --locked -- -D warnings

fmt: ## Format Rust and fix biome findings
	$(DOCKER) sh -c 'cargo fmt --all && cd extension && npx biome check --write . && cd ../desktop && npx biome check --write .'

test: ## Full test suites without coverage
	$(DOCKER) sh -c 'cargo test -p meeting --locked && cd extension && npx vitest run && cd ../desktop && npx vitest run'
	cargo test -p meeting-desktop --locked

clean-docker: ## Remove this project's hook containers, volumes and images
	-docker ps -aq --filter label=meeting-manager.hook=1 | xargs docker rm -f
	-docker volume ls -q --filter label=meeting-manager.hook=1 | xargs docker volume rm
	-docker volume rm meeting-manager-cargo meeting-manager-target meeting-manager-npm-cache
	-docker images -q --filter label=meeting-manager.hook=1 | xargs docker rmi
