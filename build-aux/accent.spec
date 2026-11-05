# `make rpm` packs the tree `make install` staged, so nothing here builds. rpm names the library
# behind each soname the binaries link by itself.
Name:           accent
Version:        %{ver}
Release:        1%{?dist}
Summary:        Markdown and PDF knowledge editor
License:        GPL-3.0-or-later
URL:            https://github.com/stroblme/accent
# libsoup's HTTPS, which no linked library names.
Requires:       glib-networking
Recommends:     git
Recommends:     openssh-clients

# No stripping and no debuginfo split: `make install` has stripped the app's binaries, their
# symbols kept out of the tree (`make pkgroot`), and each server's archive is bound to the server's
# exact bytes (build-aux/compress-server.py). libpdfium is the app's own, not one for other
# packages to link.
%global debug_package %{nil}
%global __os_install_post %{nil}
%global __provides_exclude_from ^/usr/lib/accent/

%description
Accent edits markdown and views PDFs over a vault of plain files, with wikilinks, tags,
backlinks and full-text search.

%install
cp -a %{pkgroot}/. %{buildroot}

%files
%license LICENSE
/usr/bin/accent
/usr/bin/accent-cli
/usr/lib/accent/
/usr/share/applications/io.github.stroblme.Accent.desktop
/usr/share/metainfo/io.github.stroblme.Accent.metainfo.xml
/usr/share/icons/hicolor/*/apps/io.github.stroblme.Accent*.svg
