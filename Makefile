.PHONY: all build test install install-system uninstall packages clean

CARGO ?= $(shell command -v cargo 2>/dev/null || echo "$(HOME)/.cargo/bin/cargo")

all: build

build:
	$(CARGO) build --workspace --release -j 2
	rustup target add i686-unknown-linux-gnu 2>/dev/null || true
	$(CARGO) build --manifest-path bridge_lib/Cargo.toml --target i686-unknown-linux-gnu --release -j 2 || true
	mkdir -p dist dist/32 dist/64
	cp -f target/release/luatools dist/luatools
	[ -f target/i686-unknown-linux-gnu/release/libsteam_integration.so ] && cp -f target/i686-unknown-linux-gnu/release/libsteam_integration.so dist/libsteam_integration.so && cp -f target/i686-unknown-linux-gnu/release/libsteam_integration.so dist/libsteam_integration32.so && cp -f target/i686-unknown-linux-gnu/release/libsteam_integration.so dist/32/libsteam_integration.so
	[ -f target/release/libsteam_integration.so ] && cp -f target/release/libsteam_integration.so dist/libsteam_integration64.so && cp -f target/release/libsteam_integration.so dist/64/libsteam_integration.so
	[ -f target/release/libsteam_auto_patch_linux.so ] && cp -f target/release/libsteam_auto_patch_linux.so dist/libsteam_auto_patch_linux.so

test:
	$(CARGO) test --workspace -j 2 -- --test-threads=2

install:
	bash ./install.sh --yes

install-system:
	bash ./install.sh --yes --system

uninstall:
	bash ./uninstall.sh

packages:
	bash ./packaging/build-packages.sh

clean:
	$(CARGO) clean
	rm -rf dist
