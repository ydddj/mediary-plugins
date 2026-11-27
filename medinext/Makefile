PLUGIN_ID := medinext
VERSION := 2.1.6
RUST_IMAGE := rust:1.86-bookworm
CARGO_TARGET_DIR ?= /tmp/medinext-target

.PHONY: build test package clean

build:
	@if command -v cargo >/dev/null 2>&1; then \
		CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --locked --release && \
		install -m 755 "$(CARGO_TARGET_DIR)/release/$(PLUGIN_ID)" plugin; \
	else \
		docker run --rm -v "$(CURDIR):/src" -v medifind-cargo-cache:/usr/local/cargo/registry -v medinext-target-cache:/tmp/medinext-target -e CARGO_TARGET_DIR=/tmp/medinext-target -w /src $(RUST_IMAGE) \
			sh -ec 'cargo build --release && install -m 755 /tmp/medinext-target/release/$(PLUGIN_ID) plugin'; \
	fi
	chmod 755 plugin

test:
	@if command -v cargo >/dev/null 2>&1; then \
		CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo test; \
	else \
		docker run --rm -v "$(CURDIR):/src" -v medifind-cargo-cache:/usr/local/cargo/registry -v medinext-target-cache:/tmp/medinext-target -e CARGO_TARGET_DIR=/tmp/medinext-target -w /src $(RUST_IMAGE) \
			cargo test; \
	fi

package: build
	chmod 755 plugin ui
	chmod 644 plugin.json README.md USAGE.md Cargo.toml ui/*
	mkdir -p dist && chmod 755 dist
	rm -f dist/$(PLUGIN_ID)-*-linux-amd64.tar.gz dist/SHA256SUMS
	tar -czf dist/$(PLUGIN_ID)-$(VERSION)-linux-amd64.tar.gz plugin plugin.json README.md USAGE.md ui
	chmod 644 dist/$(PLUGIN_ID)-$(VERSION)-linux-amd64.tar.gz
	cd dist && sha256sum $(PLUGIN_ID)-$(VERSION)-linux-amd64.tar.gz > SHA256SUMS && chmod 644 SHA256SUMS

clean:
	rm -rf plugin dist target
