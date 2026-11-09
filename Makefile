# devsync build tasks.
#
#   make              release build for this machine
#   make agents       static Linux builds the client uploads to remotes
#   make install      install devsync + agents under $(PREFIX) (default ~/.local)
#   make dist         release archive in dist/ (CLIENT_TARGET=<triple> to cross-build the client)
#   make check        fmt + clippy + tests
#   make docs         build the documentation site into docs/book
#   make vscode       build the VS Code extension (.vsix)

PREFIX        ?= $(HOME)/.local
VERSION       := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
HOST_TRIPLE   := $(shell rustc -vV | sed -n 's/^host: //p')
AGENT_TRIPLES ?= x86_64-unknown-linux-musl aarch64-unknown-linux-musl
CLIENT_TARGET ?=
CLIENT_TRIPLE := $(if $(CLIENT_TARGET),$(CLIENT_TARGET),$(HOST_TRIPLE))
CLIENT_BIN    := target/$(if $(CLIENT_TARGET),$(CLIENT_TARGET)/,)release/devsync
DIST_NAME     := devsync-$(VERSION)-$(CLIENT_TRIPLE)

.PHONY: all build client agents install uninstall dist checksums check test lint fmt \
        cli-docs docs docs-serve vscode clean

all: build

build:
	cargo build --release -p devsync-cli

client:
	@if [ -n "$(CLIENT_TARGET)" ]; then \
		rustup target list --installed | grep -qx $(CLIENT_TARGET) || rustup target add $(CLIENT_TARGET); \
		cargo build --release -p devsync-cli --target $(CLIENT_TARGET); \
	else \
		cargo build --release -p devsync-cli; \
	fi

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
	@echo "installed $(PREFIX)/bin/devsync"
	@case ":$$PATH:" in *":$(PREFIX)/bin:"*) ;; *) \
		echo "$(PREFIX)/bin is not on your PATH. Add it with:"; \
		echo "  echo 'export PATH=\"$(PREFIX)/bin:\$$PATH\"' >> ~/.zshrc && source ~/.zshrc";; esac

uninstall:
	rm -f $(PREFIX)/bin/devsync
	rm -rf $(PREFIX)/share/devsync

dist: client agents
	rm -rf dist/$(DIST_NAME)
	install -d dist/$(DIST_NAME)/bin dist/$(DIST_NAME)/share/devsync/agents
	install -m 755 $(CLIENT_BIN) dist/$(DIST_NAME)/bin/devsync
	@for t in $(AGENT_TRIPLES); do \
		install -m 755 target/$$t/release/devsync dist/$(DIST_NAME)/share/devsync/agents/devsync-$$t; \
	done
	cp README.md LICENSE CHANGELOG.md install.sh dist/$(DIST_NAME)/
	tar -C dist -czf dist/$(DIST_NAME).tar.gz $(DIST_NAME)
	@echo "dist/$(DIST_NAME).tar.gz"

checksums:
	cd dist && shasum -a 256 *.tar.gz *.vsix 2>/dev/null > SHA256SUMS; cat SHA256SUMS

check: fmt lint test

fmt:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

cli-docs:
	cargo build -q -p devsync-cli
	scripts/gen-cli-docs.sh target/debug/devsync > docs/src/reference/cli.md

docs:
	mdbook build docs

docs-serve:
	mdbook serve docs --open

vscode:
	cd extensions/vscode && npm ci && npm run package

clean:
	cargo clean
	rm -rf dist docs/book extensions/vscode/out extensions/vscode/*.vsix
