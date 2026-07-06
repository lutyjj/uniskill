SHELL := /bin/bash
DOCKER_IMAGE ?= uniskill-dev
CONTAINER_NAME ?= uniskill-workspace

CARGO_VOLUME ?= uniskill-cargo

.ID_FILE := .docker-container-id

define run_container
	@id=$$(cat $(.ID_FILE) 2>/dev/null || true); \
	if [ -z "$$id" ] || ! docker inspect $$id >/dev/null 2>&1; then \
		docker volume create --name=$(CARGO_VOLUME) >/dev/null 2>&1; \
		docker run -d \
			--name $(CONTAINER_NAME) \
			-v $$(pwd):/src \
			-v $(CARGO_VOLUME):/usr/local/cargo/registry \
			--entrypoint /bin/bash \
			$(DOCKER_IMAGE) -c "while true; do sleep 1; done" >/dev/null 2>&1; \
		echo $$! > $(.ID_FILE); \
	fi
endef

## Build the project
.PHONY: build
build: ensure-container
	docker exec $(CONTAINER_NAME) cargo build --release

## Run the project (pass args via ARGS=)
.PHONY: run
run: build
	target/release/uniskill $(ARGS)

## Run tests
.PHONY: test
test: ensure-container
	docker exec $(CONTAINER_NAME) cargo test

## Check formatting
.PHONY: fmt
fmt: ensure-container
	docker exec $(CONTAINER_NAME) cargo fmt -- --check

## Fix formatting
.PHONY: fmt-fix
fmt-fix: ensure-container
	docker exec $(CONTAINER_NAME) cargo fmt

## Run clippy
.PHONY: lint
lint: ensure-container
	docker exec $(CONTAINER_NAME) cargo clippy -- -D warnings

## Interactive shell in dev container
.PHONY: shell
shell: ensure-container
	docker exec -it $(CONTAINER_NAME) bash

## Rebuild the dev image
.PHONY: rebuild-image
rebuild-image:
	docker build -t $(DOCKER_IMAGE) -f docker/Dockerfile.dev .
	@rm -f $(.ID_FILE)

## Clean up container and volumes
.PHONY: clean
clean:
	@if [ -n "$$(cat $(.ID_FILE) 2>/dev/null)" ]; then \
		docker stop $(CONTAINER_NAME) >/dev/null 2>&1; \
		docker rm $(CONTAINER_NAME) >/dev/null 2>&1; \
	fi
	@rm -f $(.ID_FILE)

## Ensure container is running (shared dependency)
.PHONY: ensure-container
ensure-container:
	$(call run_container)

## Show this help
.PHONY: help
help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'
