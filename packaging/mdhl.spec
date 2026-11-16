# Built by packaging/build-rpm.sh, which creates both source tarballs.
# The crates are vendored so the build runs offline, as it would in mock.

Name:           mdhl
# Must match Cargo.toml.
Version:        0.1.0
Release:        1%{?dist}
Summary:        Highlight Markdown as ANSI-colored text on the terminal

# mdhl is GPL-3.0-or-later; the rest covers the vendored crates, as listed
# by %%cargo_license_summary during the build. packaging/check-license.sh
# (run in CI) fails when a dependency change makes this stale.
SourceLicense:  GPL-3.0-or-later
License:        GPL-3.0-or-later AND MIT AND (MIT OR Apache-2.0) AND (Unlicense OR MIT)
URL:            https://github.com/adduc/mdhl
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

ExclusiveArch:  %{rust_arches}

BuildRequires:  cargo-rpm-macros >= 26
BuildRequires:  make

%description
mdhl reads Markdown from a file or stdin and prints it to the terminal with
ANSI colors: headings, emphasis, code blocks, block quotes, lists, tables,
links and footnotes. It can wrap prose, code and tables to a set width.

%prep
%autosetup -a1
%cargo_prep -v vendor

%build
%cargo_build
%{cargo_license_summary}
%{cargo_license} > LICENSE.dependencies
%cargo_vendor_manifest

%install
# %%cargo_prep links target/release to the rpm profile's output, so the
# Makefile finds the binary where it expects it.
%make_install PREFIX=%{_prefix}

%check
%cargo_test

%files
%license LICENSE LICENSE.dependencies cargo-vendor.txt
%doc README.md
%{_bindir}/%{name}

%changelog
* Sun Nov 15 2026 John Long <a@88k.us> - 0.1.0-1
- First release
