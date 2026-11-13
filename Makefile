#!/usr/bin/make -f
#
# lowercase targets run on the host and use Docker (or Podman);
# UPPERCASE targets are what runs inside the container (or anywhere with Rust).

ifeq ($(OS),Windows_NT)
    CWD ?= "$(shell echo %CD%)"
    DOCKER ?= docker
    HOST_TARGET ?= x86_64-pc-windows-gnu
    USER_FLAG ?=
else
    CWD ?= "$(shell pwd)"
    DOCKER ?= $(if $(shell docker -v 2>/dev/null),docker,podman)
    UNAME_S := $(shell uname -s)
    ARCH := $(patsubst arm64,aarch64,$(shell uname -m))
    ifeq ($(UNAME_S),Darwin)
        HOST_TARGET ?= $(ARCH)-apple-darwin
    else
        HOST_TARGET ?= $(ARCH)-unknown-linux-gnu
    endif
    # Run as you, so files written to the mounted source stay yours
    ifeq ($(DOCKER),podman)
        USER_FLAG ?= --userns=keep-id
    else
        USER_FLAG ?= --user $(shell id -u):$(shell id -g)
    endif
endif

DOCKER_IMAGE_TAG ?= snshn/insider
TARGET_VOLUME ?= insider-target
TARGET ?= $(HOST_TARGET)
RELEASE_TARGETS ?= aarch64-apple-darwin x86_64-apple-darwin \
                   aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu \
                   x86_64-pc-windows-gnu
OUT ?= out/$(TARGET)
EXE = $(if $(findstring windows,$(TARGET)),.exe,)
BIN = out/$(TARGET)/insider$(EXE)
PREFIX ?= /usr/local
CARGO_TARGET_DIR ?= target
TTY := $(shell test -t 0 && echo -t)

RUN_DEV = $(DOCKER) run --rm -i $(TTY) $(USER_FLAG) \
	-v $(CWD):/src/$(DOCKER_IMAGE_TAG) \
	-v $(TARGET_VOLUME):/target \
	$(DOCKER_IMAGE_TAG)

# Apple silicon refuses unsigned binaries; an ad-hoc signature is enough
define SIGN
	@if [ "$(UNAME_S)" = Darwin ] && echo "$(TARGET)" | grep -q apple-darwin \
	    && command -v codesign >/dev/null; then \
	    codesign --force --sign - $(BIN); \
	fi
endef

.DEFAULT_GOAL := help

help: ## Show this helpful message
	@for ML in $(MAKEFILE_LIST); do \
		grep -E '^[a-zA-Z_-]+:.*?## .*$$' $$ML | sort | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'; \
	done
	@echo
	@echo "TARGET defaults to this machine ($(HOST_TARGET)); e.g. make build TARGET=x86_64-pc-windows-gnu"
.PHONY: help

# --- host: extract ------------------------------------------------------------

build: ## Build in a throwaway container, extract the binary to ./out/<target>/
	@$(DOCKER) build --target export --build-arg TARGET=$(TARGET) --output type=local,dest=out/$(TARGET) .
	$(SIGN)
	@echo "built $(BIN)"
.PHONY: build

dist: ## Build every release target into ./out/
	@for t in $(RELEASE_TARGETS); do $(MAKE) --no-print-directory build TARGET=$$t || exit 1; done
.PHONY: dist

install: ## Copy ./out/<this machine>/insider to $(PREFIX)/bin (run make build first)
	@test -f $(BIN) || { echo "no $(BIN): run 'make build' first"; exit 1; }
	@install -d $(DESTDIR)$(PREFIX)/bin
	@install -m 0755 $(BIN) $(DESTDIR)$(PREFIX)/bin/insider$(EXE)
	@echo "installed $(DESTDIR)$(PREFIX)/bin/insider$(EXE)"
.PHONY: install

uninstall: ## Remove $(PREFIX)/bin/insider
	@rm -f $(DESTDIR)$(PREFIX)/bin/insider$(EXE)
.PHONY: uninstall

# --- host: -v mounted development -------------------------------------------------

image: ## Create the toolchain image
	@$(DOCKER) build --target toolchain -t $(DOCKER_IMAGE_TAG) .
.PHONY: image

dev: image ## Incremental build with the source mounted, binary to ./out/<target>/
	@$(RUN_DEV) make BUILD TARGET=$(TARGET)
	$(SIGN)
	@echo "built $(BIN)"
.PHONY: dev

run: dev ## Incremental build, then run it on this machine
	@./$(BIN)
.PHONY: run

test: image ## Run the tests in a container
	@$(RUN_DEV) make TEST
.PHONY: test

lint: image ## Check formatting and run clippy in a container
	@$(RUN_DEV) make LINT
.PHONY: lint

fmt: image ## Format the source in a container
	@$(RUN_DEV) cargo fmt
.PHONY: fmt

shell: image ## Open a shell in the toolchain container
	@$(RUN_DEV) bash
.PHONY: shell

update-lock-file: image ## Update Cargo.lock
	@$(RUN_DEV) cargo update
.PHONY: update-lock-file

clean: ## Remove ./out and the build-cache volume
	@rm -rf out
	@-$(DOCKER) volume rm $(TARGET_VOLUME) >/dev/null 2>&1
.PHONY: clean

# --- inside the container -------------------------------------------------------

BUILD: ## Cross-compile for $(TARGET) into $(OUT)
	@cargo zigbuild --locked --release --target $(TARGET)
	@mkdir -p $(OUT)
	@cp "$(CARGO_TARGET_DIR)/$(TARGET)/release/insider$(EXE)" $(OUT)/
.PHONY: BUILD

TEST: ## Run tests
	@cargo test --locked
.PHONY: TEST

LINT: ## Check formatting, run clippy
	@cargo fmt --check
	@cargo clippy --locked --all-targets
.PHONY: LINT
