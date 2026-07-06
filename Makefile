PROJECT          := uniskill
RUST_VERSION     ?= 1.96.1
TARGET_TRIPLE    ?= $(shell scripts/host-triple.sh)
INSTALL_DIR      ?= $(HOME)/.local/bin
DIST_DIR         ?= dist
PACKAGE_NAME     ?= $(PROJECT)-$(TARGET_TRIPLE)

RELEASE_BIN      := target/$(TARGET_TRIPLE)/release/$(PROJECT)
DIST_BIN         := $(DIST_DIR)/$(PACKAGE_NAME)
DIST_TARBALL     := $(DIST_DIR)/$(PACKAGE_NAME).tar.gz
DIST_SHA256      := $(DIST_DIR)/$(PACKAGE_NAME).sha256

## Build release binary (auto-formats first)
.PHONY: build
build: fmt-fix
	@echo ">> building $(TARGET_TRIPLE) (release)"
	cargo build --release --target $(TARGET_TRIPLE)
	@echo "✓ → $(RELEASE_BIN)"

## Build debug binary (fast iteration)
.PHONY: dev
dev:
	@echo ">> building $(TARGET_TRIPLE) (debug)"
	cargo build --target $(TARGET_TRIPLE)

## Pre-release gate: fmt-check + clippy + test + package (mirrors CI)
.PHONY: release
release: fmt-check clippy test package
	@echo "✓ release-ready → $(DIST_BIN), $(DIST_TARBALL)"

## Build release binary without formatting (used by release target and CI)
.PHONY: build-only
build-only:
	@echo ">> building $(TARGET_TRIPLE) (release)"
	cargo build --release --target $(TARGET_TRIPLE)

## Build release assets in DIST_DIR: raw binary, tarball, checksum file
.PHONY: package
package: build-only
	@echo ">> packaging $(PACKAGE_NAME)"
	@mkdir -p $(DIST_DIR) $(DIST_DIR)/staging/$(PACKAGE_NAME)
	cp $(RELEASE_BIN) $(DIST_BIN)
	chmod 755 $(DIST_BIN)
	cp $(RELEASE_BIN) $(DIST_DIR)/staging/$(PACKAGE_NAME)/$(PROJECT)
	chmod 755 $(DIST_DIR)/staging/$(PACKAGE_NAME)/$(PROJECT)
	tar czf $(DIST_TARBALL) -C $(DIST_DIR)/staging/$(PACKAGE_NAME) $(PROJECT)
	shasum -a 256 $(DIST_BIN) $(DIST_TARBALL) > $(DIST_SHA256)
	@echo "✓ packaged → $(DIST_BIN)"
	@echo "✓ packaged → $(DIST_TARBALL)"

## Run tests
.PHONY: test
test:
	@echo ">> testing"
	cargo test --all-features

## Check formatting (fails on drift)
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
	@echo "DIST_DIR      = $(DIST_DIR)"
	@echo "PACKAGE_NAME  = $(PACKAGE_NAME)"
	@rustc --version 2>/dev/null || echo "rustc not found"

## Show help
.PHONY: help
help:
	@awk '/^## / { help = substr($$0, 4); next } \
		/^[a-zA-Z_-]+:/ { split($$1, target, ":"); if (help) { printf "  \033[36m%-15s\033[0m %s\n", target[1], help; help = "" } }' \
		$(MAKEFILE_LIST)
