PROJECT          := uniskill
RUST_VERSION     ?= 1.96.1
TARGET_TRIPLE    ?= $(shell scripts/host-triple.sh)
INSTALL_DIR      ?= $(HOME)/.local/bin

RELEASE_BIN      := target/$(TARGET_TRIPLE)/release/$(PROJECT)

## Build release binary for the host platform
.PHONY: build
build:
	@echo ">> building $(TARGET_TRIPLE) (release)"
	cargo build --release --target $(TARGET_TRIPLE)
	@echo "✓ → $(RELEASE_BIN)"

## Build debug binary (fast iteration)
.PHONY: dev
dev:
	@echo ">> building $(TARGET_TRIPLE) (debug)"
	cargo build --target $(TARGET_TRIPLE)

## Run tests
.PHONY: test
test:
	@echo ">> testing"
	cargo test --all-features

## Check formatting
.PHONY: fmt-check
fmt-check:
	cargo fmt -- --check

## Fix formatting
.PHONY: fmt-fix
fmt-fix:
	cargo fmt

## Run clippy lint
.PHONY: clippy
clippy:
	cargo clippy --all-features -- -D warnings

## Build release and copy to INSTALL_DIR
.PHONY: install
install: build
	@mkdir -p $(INSTALL_DIR)
	cp $(RELEASE_BIN) $(INSTALL_DIR)/$(PROJECT)
	@echo "✓ installed → $(INSTALL_DIR)/$(PROJECT)"

## Clean build artifacts
.PHONY: clean
clean:
	cargo clean

## Print config
.PHONY: info
info:
	@echo "TARGET_TRIPLE = $(TARGET_TRIPLE)"
	@echo "RUST_VERSION  = $(RUST_VERSION)"
	@echo "INSTALL_DIR   = $(INSTALL_DIR)"
	@rustc --version 2>/dev/null || echo "rustc not found"

## Show help
.PHONY: help
help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'
