# devsync build tasks.
#
#   make            release build for this machine
#   make agents     static Linux builds the client uploads to remotes
#   make install    install devsync + agents under $(PREFIX) (default ~/.local)
#   make dist       release tarball in dist/
#   make check      fmt + clippy + tests
#   make vscode     build the VS Code extension (.vsix)

PREFIX        ?= $(HOME)/.local
VERSION       := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
HOST_TRIPLE   := $(shell rustc -vV | sed -n 's/^host: //p')
AGENT_TRIPLES ?= x86_64-unknown-linux-musl aarch64-unknown-linux-musl
DIST_NAME     := devsync-$(VERSION)-$(HOST_TRIPLE)

.PHONY: all build agents install uninstall dist check test lint fmt vscode clean

all: build

build:
	cargo build --release -p devsync-cli

agents:
	@for t in $(AGENT_TRIPLES); do \
		rustup target list --installed | grep -qx $$t || rustup target add $$t; \
		echo "building agent for $$t"; \
		cargo build --release -p devsync-cli --target $$t || exit 1; \
	done

install: build agents
	install -d $(PREFIX)/bin $(PREFIX)/share/devsync/agents
	install -m 755 target/release/devsync $(PREFIX)/bin/devsync
	@for t in $(AGENT_TRIPLES); do \
		install -m 755 target/$$t/release/devsync $(PREFIX)/share/devsync/agents/devsync-$$t; \
	done
	@echo "installed to $(PREFIX)/bin/devsync — make sure $(PREFIX)/bin is on your PATH"

uninstall:
	rm -f $(PREFIX)/bin/devsync
	rm -rf $(PREFIX)/share/devsync

dist: build agents
	rm -rf dist/$(DIST_NAME)
	install -d dist/$(DIST_NAME)/bin dist/$(DIST_NAME)/share/devsync/agents
	install -m 755 target/release/devsync dist/$(DIST_NAME)/bin/
	@for t in $(AGENT_TRIPLES); do \
		install -m 755 target/$$t/release/devsync dist/$(DIST_NAME)/share/devsync/agents/devsync-$$t; \
	done
	cp README.md LICENSE scripts/install.sh dist/$(DIST_NAME)/
	tar -C dist -czf dist/$(DIST_NAME).tar.gz $(DIST_NAME)
	@echo "dist/$(DIST_NAME).tar.gz"

check: fmt lint test

fmt:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

vscode:
	cd extensions/vscode && npm ci && npm run package

clean:
	cargo clean
	rm -rf dist extensions/vscode/out extensions/vscode/*.vsix
