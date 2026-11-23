.PHONY: all build test install install-system uninstall packages clean

CARGO ?= $(shell command -v cargo 2>/dev/null || echo "$(HOME)/.cargo/bin/cargo")

all: build

build:
	$(CARGO) build --workspace --release -j 2
	$(CARGO) build --profile debug-debug -p bridge_lib -j 2

test:
	$(CARGO) test --workspace

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
