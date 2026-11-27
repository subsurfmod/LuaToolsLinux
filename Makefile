.PHONY: all build test install install-system uninstall packages clean

CARGO ?= $(shell command -v cargo 2>/dev/null || echo "$(HOME)/.cargo/bin/cargo")

all: build

build:
	$(CARGO) build --workspace --release -j 2
	rustup target add i686-unknown-linux-gnu 2>/dev/null || true
	$(CARGO) build --manifest-path bridge_lib/Cargo.toml --target i686-unknown-linux-gnu --release -j 2 || true
	mkdir -p dist dist/32 dist/64 dist/lib dist/lib64
	cp -f target/release/luatools dist/luatools
	if [ -f target/i686-unknown-linux-gnu/release/libsteam_integration.so ]; then \
		cp -f target/i686-unknown-linux-gnu/release/libsteam_integration.so dist/libsteam_integration.so; \
		cp -f target/i686-unknown-linux-gnu/release/libsteam_integration.so dist/libsteam_integration32.so; \
		cp -f target/i686-unknown-linux-gnu/release/libsteam_integration.so dist/32/libsteam_integration.so; \
		cp -f target/i686-unknown-linux-gnu/release/libsteam_integration.so dist/lib/libsteam_integration.so; \
		rm -rf dist/lib32 dist/i386-linux-gnu dist/lib/i386-linux-gnu; \
		ln -sfn lib dist/lib32; \
		ln -sfn lib dist/i386-linux-gnu; \
		ln -sfn ../32 dist/lib/i386-linux-gnu; \
	elif [ -f target/release/libsteam_integration.so ]; then \
		cp -f target/release/libsteam_integration.so dist/libsteam_integration.so; \
	fi
	if [ -f target/release/libsteam_integration.so ]; then \
		cp -f target/release/libsteam_integration.so dist/libsteam_integration64.so; \
		cp -f target/release/libsteam_integration.so dist/64/libsteam_integration.so; \
		cp -f target/release/libsteam_integration.so dist/lib64/libsteam_integration.so; \
		rm -rf dist/x86_64-linux-gnu dist/lib/x86_64-linux-gnu; \
		ln -sfn lib64 dist/x86_64-linux-gnu; \
		ln -sfn ../lib64 dist/lib/x86_64-linux-gnu; \
	fi
	if [ -f target/release/libsteam_auto_patch_linux.so ]; then \
		cp -f target/release/libsteam_auto_patch_linux.so dist/libsteam_auto_patch_linux.so; \
	fi

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
