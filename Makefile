.PHONY: all build test install install-system uninstall packages clean

CARGO ?= $(shell command -v cargo 2>/dev/null || echo "$(HOME)/.cargo/bin/cargo")

all: build

build:
	$(CARGO) build --release

test:
	$(CARGO) test

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
