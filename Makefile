# mdhl
#
#   make                 build a release binary
#   make test            run the test suite
#   make install         install the binary
#   make uninstall       remove it again
#   make clean           remove build output
#   make rpm             build a Fedora RPM (see packaging/build-rpm.sh)
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

.PHONY: all build test install uninstall clean rpm

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
