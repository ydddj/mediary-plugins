PLUGIN_ID := mediwash
VERSION := 3.2.0
RUST_IMAGE := rust:1.86-bookworm
CARGO_TARGET_DIR ?= /tmp/mediwash-target

.PHONY: build test package publish clean

build:
	@if command -v cargo >/dev/null 2>&1; then \
		CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo build --locked --release && \
		install -m 755 "$(CARGO_TARGET_DIR)/release/$(PLUGIN_ID)" plugin; \
	else \
		docker run --rm -v "$(CURDIR):/src" -v medifind-cargo-cache:/usr/local/cargo/registry -v mediwash-target-cache:/tmp/mediwash-target -e CARGO_TARGET_DIR=/tmp/mediwash-target -w /src $(RUST_IMAGE) \
			sh -ec 'cargo build --locked --release && install -m 755 /tmp/mediwash-target/release/$(PLUGIN_ID) plugin'; \
	fi
	chmod 755 plugin

test:
	@if command -v cargo >/dev/null 2>&1; then \
		CARGO_TARGET_DIR="$(CARGO_TARGET_DIR)" cargo test --locked; \
	else \
		docker run --rm -v "$(CURDIR):/src" -v medifind-cargo-cache:/usr/local/cargo/registry -v mediwash-target-cache:/tmp/mediwash-target -e CARGO_TARGET_DIR=/tmp/mediwash-target -w /src $(RUST_IMAGE) \
			cargo test --locked; \
	fi

package: build
	chmod 755 plugin ui
	chmod 644 plugin.json README.md USAGE.md Cargo.toml ui/*
	mkdir -p dist && chmod 755 dist
	tar -czf dist/$(PLUGIN_ID)-$(VERSION)-linux-amd64.tar.gz plugin plugin.json README.md USAGE.md ui
	chmod 644 dist/$(PLUGIN_ID)-$(VERSION)-linux-amd64.tar.gz
	cd dist && sha256sum $(PLUGIN_ID)-$(VERSION)-linux-amd64.tar.gz > SHA256SUMS && chmod 644 SHA256SUMS

publish: package
	@if [ -f scripts/publish_github.py ]; then \
		python3 scripts/publish_github.py --plugins $(PLUGIN_ID); \
	elif [ -f ../scripts/publish_github.py ]; then \
		python3 ../scripts/publish_github.py --plugins $(PLUGIN_ID); \
	fi

clean:
	rm -rf plugin dist target
