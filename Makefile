PROJECT          := uniskill
RUST_VERSION     ?= 1.96.1
DOCKER_IMAGE     ?= uniskill-build
CARGO_CACHE_VOLUME?= $(PROJECT)-cargo-cache
PLATFORM         ?= linux/amd64

# Default target: x86_64-unknown-linux-gnu
TARGET_TRIPLE    ?= x86_64-unknown-linux-gnu

CARGO_CACHE      := /usr/local/cargo/registry

define _build
	@docker run --rm \
		--platform $(PLATFORM) \
		-v $(CURDIR):/src \
		-v $(CARGO_CACHE_VOLUME):$(CARGO_CACHE) \
		-w /src \
		-e CC=gcc \
		$(DOCKER_IMAGE) \
		bash -c "rustup default $(RUST_VERSION) && rustup target add $(TARGET_TRIPLE) && cargo clean && exec cargo $1 --target $(TARGET_TRIPLE)"
endef

## Build. Default: x86_64-unknown-linux-gnu. Override with TARGET_TRIPLE=<triple>.
.PHONY: build
build:
	@mkdir -p target/$(TARGET_TRIPLE)/release
	@echo ">> building $(TARGET_TRIPLE)"
	$(call _build,build --release)
	@echo "✓ → target/$(TARGET_TRIPLE)/release/$(PROJECT)"

## Run tests for TARGET_TRIPLE (cross-test only; does not execute binaries)
.PHONY: test
test:
	@echo ">> testing $(TARGET_TRIPLE)"
	$(call _build,test --all-features)

## Check formatting
.PHONY: fmt-check
fmt-check:
	$(call _build,fmt -- --check)

## Fix formatting
.PHONY: fmt-fix
fmt-fix:
	$(call _build,fmt)

## Run clippy lint
.PHONY: clippy
clippy:
	$(call _build,clippy --all-features -- -D warnings)

## Clean build artifacts for current target
.PHONY: clean
clean:
	rm -rf target/$(TARGET_TRIPLE)/release

## Drop cargo registry cache volume
.PHONY: clean-cache
clean-cache:
	docker volume rm $(CARGO_CACHE_VOLUME) 2>/dev/null || true

## Print config
.PHONY: info
info:
	@echo "PLATFORM    = $(PLATFORM)"
	@echo "TARGET_TRIPLE = $(TARGET_TRIPLE)"
	@echo "RUST_VERSION  = $(RUST_VERSION)"
	@echo "DOCKER_IMAGE  = $(DOCKER_IMAGE)"

## Show help
.PHONY: help
help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'
