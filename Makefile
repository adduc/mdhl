# mdhl
#
#   make                 build a release binary
#   make test            run the test suite
#   make install         install the binary
#   make uninstall       remove it again
#   make clean           remove build output
#   make rpm             build a Fedora RPM (see packaging/build-rpm.sh)
#   make dist            build a static Linux tarball in target/dist/
#                        (see packaging/build-tarball.sh; set RUST_TARGET to
#                        cross-build, e.g. RUST_TARGET=aarch64-unknown-linux-musl)
#
# Installs into ~/.local by default, so no root is needed. For a system-wide
# install, override PREFIX:
#
#   make && sudo make install PREFIX=/usr/local
#
# DESTDIR is honored for staged installs (packaging).

PREFIX  ?= $(HOME)/.local
BINDIR  ?= $(PREFIX)/bin

CARGO   ?= cargo
INSTALL ?= install

BIN     := mdhl
TARGET  := target/release/$(BIN)

.PHONY: all build test install uninstall clean rpm dist

all: build

# Cargo does its own dependency tracking, so always hand off to it.
build:
	$(CARGO) build --release

test:
	$(CARGO) test

# Doesn't build, so `sudo make install` never runs cargo as root.
install:
	@test -x $(TARGET) || { echo "$(TARGET) not found; run 'make' first" >&2; exit 1; }
	$(INSTALL) -Dm755 $(TARGET) $(DESTDIR)$(BINDIR)/$(BIN)

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/$(BIN)

clean:
	$(CARGO) clean

# Needs rpm-build and cargo-rpm-macros. Output lands in target/rpmbuild/.
rpm:
	packaging/build-rpm.sh

# Needs the musl target: rustup target add $(uname -m)-unknown-linux-musl
dist:
	packaging/build-tarball.sh $(RUST_TARGET)
