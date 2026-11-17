#!/bin/sh
# web-install.sh - installs Recipe (package recipe-pm, command `recipe`) on an x86_64 Linux.
#
# It looks at /etc/os-release, picks one package type (rpm, deb, apk or tarball), finds the
# newest release on GitHub, downloads the matching file, checks its SHA-256 against the
# release's SHA256SUMS, and installs it with the system's own tool. It needs root only for
# the install step (root, else sudo, else doas).
#
# The classification (families by ID, then ID_LIKE, then the package tool that exists) is
# modelled on Tailscale's public install script, https://tailscale.com/install.sh (BSD
# licence); the lists and the code here are our own.
#
# One line:
#     curl -fsSL https://levitateos.org/recipe/install.sh | sh
#
# Download, read, then run (the same thing, with a look in between):
#     curl -fsSL https://levitateos.org/recipe/install.sh -o recipe-install.sh
#     less recipe-install.sh
#     sh recipe-install.sh
#
# Options:
#     --dry-run       print the type chosen and every command that would run; no network
#                     request, nothing changed
#     --allow-nixos   continue on NixOS (the tarball goes to /usr/local, which NixOS does not
#                     put on PATH by default)
#
# Environment variables:
#     RECIPE_INSTALL_TYPE=rpm|deb|apk|tarball   force the package type
#     RECIPE_VERSION=<v>                        install release <v> (a tag such as 0.2.0)
#                                               instead of the newest one
#     RECIPE_INSTALL_DRY_RUN=1                  same as --dry-run
#     RECIPE_OS_RELEASE=<file>                  read this file instead of /etc/os-release
#                                               (for checking the classification)
#
# The six release formats (a tarball with install.sh and uninstall.sh, RPM, .deb, APK, AUR
# files, a Void template, plus a -usr tarball for packagers) are made by
# `cargo xtask release recipe` and described in docs/recipe-standalone-release.md of the
# LevitateOS repository. Only rpm, deb, apk and the tarball are used here; Arch and Void
# users get the tarball, or the AUR / Void packages.

set -eu

REPO_URL="https://github.com/LevitateOS/recipe"
PKG="recipe-pm"

say() { printf '%s\n' "$*"; }
warn() { printf 'recipe install: %s\n' "$*" >&2; }
die() {
	printf 'recipe install: %s\n' "$*" >&2
	exit 1
}

have() { command -v "$1" >/dev/null 2>&1; }

# Value of KEY in the os-release file, without quotes. Parsed, not sourced.
osval() {
	[ -r "$OS_FILE" ] || return 0
	sed -n "s/^$1=//p" "$OS_FILE" | head -n 1 | sed -e 's/^"//' -e 's/"$//' -e "s/^'//" -e "s/'\$//"
}

# True when /usr is mounted read-only.
usr_is_readonly() {
	[ -r /proc/mounts ] || return 1
	awk '$2 == "/usr" && $4 ~ /(^|,)ro(,|$)/ { found = 1 } END { exit !found }' /proc/mounts
}

# Sets TYPE, REASON (and NIXOS=1 on NixOS) from the system. Does not look at RECIPE_INSTALL_TYPE.
classify() {
	TYPE=""
	REASON=""
	NIXOS=0

	if [ "$ID" = "nixos" ]; then
		NIXOS=1
		TYPE="tarball"
		REASON="NixOS: no native package is published"
		return 0
	fi

	# Image-based systems: /usr is read-only, so a native package cannot be installed there.
	# The tarball installs into /usr/local, which these systems keep writable.
	case " $ID " in
	" steamos "|" bazzite "|" bluefin "|" aurora "|" kde-linux "|" opensuse-microos "|" opensuse-aeon "|" opensuse-kalpa "|" sle-micro "|" sle-micro-rancher ")
		TYPE="tarball"
		REASON="image-based system ($ID): /usr is read-only"
		return 0
		;;
	esac
	case " $VARIANT_ID " in
	" silverblue "|" kinoite "|" sericea "|" onyx "|" cosmic-atomic "|" coreos "|" iot ")
		TYPE="tarball"
		REASON="Fedora Atomic variant ($VARIANT_ID): /usr is read-only"
		return 0
		;;
	esac
	if [ -e /run/ostree-booted ]; then
		TYPE="tarball"
		REASON="ostree-based system (/run/ostree-booted exists): /usr is read-only"
		return 0
	fi
	if usr_is_readonly; then
		TYPE="tarball"
		REASON="/usr is mounted read-only"
		return 0
	fi

	# 1. By ID.
	case "$ID" in
	debian|ubuntu|pop|neon|tuxedo|linuxmint|elementary|industrial-os|parrot|mendel|galliumos|pureos|kaisen|raspbian|kali|Deepin|deepin|pika|sparky|osmc|zorin|devuan)
		TYPE="deb"
		REASON="ID=$ID is a Debian-family system"
		return 0
		;;
	fedora|rhel|centos|ol|miraclelinux|rocky|almalinux|nobara|openmandriva|sangoma|risios|cloudlinux|alinux|fedora-asahi-remix|ultramarine|amzn|xenenterprise|opensuse|opensuse-leap|opensuse-tumbleweed|opensuse-slowroll|sles|photon)
		TYPE="rpm"
		REASON="ID=$ID is an RPM-family system"
		return 0
		;;
	alpine|postmarketos)
		TYPE="apk"
		REASON="ID=$ID is an Alpine-family system"
		return 0
		;;
	arch|archarm|endeavouros|blendos|garuda|archcraft|cachyos|omarchy|manjaro|manjaro-arm|biglinux)
		TYPE="tarball"
		REASON="ID=$ID is an Arch-family system: no pacman package is published (the AUR has recipe-pm)"
		return 0
		;;
	void|gentoo)
		TYPE="tarball"
		REASON="ID=$ID: no native package is published"
		return 0
		;;
	esac

	# 2. By the words of ID_LIKE.
	for word in $ID_LIKE; do
		case "$word" in
		debian|ubuntu)
			TYPE="deb"
			REASON="ID_LIKE contains $word"
			return 0
			;;
		fedora|rhel|centos|suse|opensuse)
			TYPE="rpm"
			REASON="ID_LIKE contains $word"
			return 0
			;;
		alpine)
			TYPE="apk"
			REASON="ID_LIKE contains $word"
			return 0
			;;
		arch)
			TYPE="tarball"
			REASON="ID_LIKE contains arch: no pacman package is published"
			return 0
			;;
		esac
	done

	# 3. By the package tool that exists.
	if have dnf || have yum || have zypper || have tdnf; then
		TYPE="rpm"
		REASON="found an RPM package tool"
	elif have apt-get; then
		TYPE="deb"
		REASON="found apt-get"
	elif have apk; then
		TYPE="apk"
		REASON="found apk"
	fi
	return 0
}

kernel_check() {
	kver=$(uname -r)
	major=${kver%%.*}
	rest=${kver#*.}
	minor=${rest%%[!0-9]*}
	case "$major$minor" in
	"" | *[!0-9]*) return 0 ;;
	esac
	if [ "$major" -lt 6 ] || { [ "$major" -eq 6 ] && [ "$minor" -lt 10 ]; }; then
		warn "warning: kernel $kver is older than 6.10; Recipe falls back to the /proc/self/fd path there (tried on 6.8), and kernels in the 5.x series are untried."
	fi
}

# Download URL $1 to file $2.
fetch() {
	if have curl; then
		curl -fsSL "$1" -o "$2"
	else
		wget -q -O "$2" "$1"
	fi
}

sha256_of() {
	if have sha256sum; then
		sha256sum "$1" | cut -d ' ' -f 1
	elif have shasum; then
		shasum -a 256 "$1" | cut -d ' ' -f 1
	elif have openssl; then
		openssl dgst -sha256 "$1" | sed 's/^.*= //'
	else
		return 1
	fi
}

# Prints the command (as the install step), then runs it unless this is a dry run.
run() {
	say "+ $*"
	[ "$DRY" = 1 ] && return 0
	"$@" </dev/null
}

run_root() {
	if [ -n "$SUDO" ]; then
		run "$SUDO" "$@"
	else
		run "$@"
	fi
}

main() {
	DRY=0
	ALLOW_NIXOS=0
	case "${RECIPE_INSTALL_DRY_RUN:-}" in "" | 0) ;; *) DRY=1 ;; esac
	for arg in "$@"; do
		case "$arg" in
		--dry-run) DRY=1 ;;
		--allow-nixos) ALLOW_NIXOS=1 ;;
		*) die "unknown option: $arg (known: --dry-run, --allow-nixos)" ;;
		esac
	done

	[ "$(uname -s)" = "Linux" ] || die "Recipe installs on Linux only; this system is $(uname -s)."
	arch=$(uname -m)
	case "$arch" in
	x86_64 | amd64) ;;
	*) die "Recipe is published for x86_64 only; this machine is $arch." ;;
	esac
	if [ "$DRY" = 0 ] && ! have curl && ! have wget; then
		die "neither curl nor wget is installed; install one of them and run this again."
	fi

	kernel_check

	OS_FILE=${RECIPE_OS_RELEASE:-/etc/os-release}
	[ -r "$OS_FILE" ] || OS_FILE=/usr/lib/os-release
	ID=$(osval ID)
	ID_LIKE=$(osval ID_LIKE)
	VARIANT_ID=$(osval VARIANT_ID)

	NIXOS=0
	if [ -n "${RECIPE_INSTALL_TYPE:-}" ]; then
		case "$RECIPE_INSTALL_TYPE" in
		rpm | deb | apk | tarball) ;;
		*) die "RECIPE_INSTALL_TYPE must be rpm, deb, apk or tarball, not '$RECIPE_INSTALL_TYPE'." ;;
		esac
		TYPE=$RECIPE_INSTALL_TYPE
		REASON="forced by RECIPE_INSTALL_TYPE"
	else
		classify
	fi

	if [ -z "$TYPE" ]; then
		warn "could not tell which package type this system uses."
		warn "found: ID='$ID' ID_LIKE='$ID_LIKE' uname='$(uname -srm)'"
		warn "the tarball works on any x86_64 Linux. To use it, run this again with:"
		warn "    RECIPE_INSTALL_TYPE=tarball (put it before 'sh', as in: curl -fsSL https://levitateos.org/recipe/install.sh | RECIPE_INSTALL_TYPE=tarball sh)"
		exit 1
	fi

	say "Package type: $TYPE ($REASON)"

	if [ "$NIXOS" = 1 ] && [ "$ALLOW_NIXOS" = 0 ]; then
		warn "NixOS: the tarball installs into /usr/local, which NixOS does not put on PATH by default,"
		warn "so 'recipe' would not be found until you add /usr/local/bin to your PATH."
		warn "To go on anyway, run this again with --allow-nixos (for example: curl -fsSL https://levitateos.org/recipe/install.sh | sh -s -- --allow-nixos)."
		[ "$DRY" = 1 ] || exit 1
		say "(dry run: would stop here without --allow-nixos)"
	fi

	# Which file of the release.
	case "$TYPE" in
	rpm) pattern="$PKG-*-1.x86_64.rpm" ;;
	deb) pattern="${PKG}_*-1_amd64.deb" ;;
	apk) pattern="$PKG-*-r0.x86_64.apk" ;;
	tarball) pattern="$PKG-*-x86_64.tar.zst" ;; # the -usr tarball ends in -x86_64-usr.tar.zst
	esac
	if [ -n "${RECIPE_VERSION:-}" ]; then
		base="$REPO_URL/releases/download/$RECIPE_VERSION"
	else
		base="$REPO_URL/releases/latest/download"
	fi

	# Who installs.
	SUDO=""
	if [ "$(id -u)" != 0 ]; then
		if have sudo; then
			SUDO=sudo
		elif have doas; then
			SUDO=doas
		fi
	fi

	# The tool that installs a package.
	INSTALLER=""
	case "$TYPE" in
	rpm)
		for t in dnf yum zypper tdnf; do
			if have "$t"; then
				INSTALLER=$t
				break
			fi
		done
		[ -n "$INSTALLER" ] || [ "$DRY" = 1 ] || die "no RPM package tool found (dnf, yum, zypper, tdnf); install one, or use RECIPE_INSTALL_TYPE=tarball."
		[ -n "$INSTALLER" ] || INSTALLER=dnf
		;;
	deb)
		have apt-get || [ "$DRY" = 1 ] || die "apt-get not found; use RECIPE_INSTALL_TYPE=tarball."
		;;
	apk)
		have apk || [ "$DRY" = 1 ] || die "apk not found; use RECIPE_INSTALL_TYPE=tarball."
		;;
	tarball)
		if have zstd; then
			UNPACK=zstd
		elif have tar && tar --help 2>&1 | grep -qi zstd; then
			UNPACK=tar
		elif [ "$DRY" = 1 ]; then
			UNPACK=tar
		else
			die "unpacking the tarball needs zstd, or a tar with zstd support; install the 'zstd' package (apt install zstd, dnf install zstd, apk add zstd) and run this again."
		fi
		have tar || [ "$DRY" = 1 ] || die "tar not found; install the 'tar' package and run this again."
		;;
	esac

	if [ "$DRY" = 1 ]; then
		say "Dry run: no network request, nothing changed. Steps that would run:"
		say "  1. fetch $base/SHA256SUMS and take the line for $pattern"
		say "  2. fetch $base/<that file> into a private temporary folder; check its SHA-256"
		file="<$pattern>"
		tmp="<temporary folder>"
		if [ "$(id -u)" != 0 ] && [ -z "$SUDO" ]; then
			say "  (not root, and neither sudo nor doas exists: a real run would stop and ask you to run the install as root)"
		fi
		say "  3. install:"
	else
		tmp=$(mktemp -d "${TMPDIR:-/tmp}/recipe-install.XXXXXX") || die "could not make a temporary folder."
		trap 'rm -rf "$tmp"' EXIT
		trap 'exit 1' INT TERM HUP
		chmod 0755 "$tmp" # so apt's sandbox user can read the package

		say "Finding the release ..."
		if ! fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"; then
			die "no release could be found at $base/SHA256SUMS. See the releases page: $REPO_URL/releases"
		fi
		file=""
		want=""
		while read -r sum name rest; do
			# `*name` is how sha256sum marks binary mode
			name=${name#\*}
			case "$name" in
			$pattern)
				file=$name
				want=$sum
				;;
			esac
		done <"$tmp/SHA256SUMS"
		if [ -z "$file" ] || [ -z "$want" ]; then
			die "this release has no file named like $pattern (see $REPO_URL/releases)."
		fi

		say "Downloading $file ..."
		fetch "$base/$file" "$tmp/$file" || die "could not download $base/$file"
		have_sum=$(sha256_of "$tmp/$file") || die "no sha256sum, shasum or openssl found, so the download cannot be checked; install coreutils and run this again."
		if [ "$have_sum" != "$want" ]; then
			die "the SHA-256 of $file is $have_sum but SHA256SUMS says $want; not installing."
		fi
		say "SHA-256 matches: $want"

		if [ "$(id -u)" != 0 ] && [ -z "$SUDO" ]; then
			warn "installing needs root, and neither sudo nor doas exists. Run this as root:"
			case "$TYPE" in
			rpm) warn "    dnf install -y $tmp/$file    (or the RPM tool of your system)" ;;
			deb) warn "    apt-get install -y $tmp/$file" ;;
			apk) warn "    apk add --allow-untrusted $tmp/$file" ;;
			tarball) warn "    unpack $tmp/$file and run: sh install.sh   (inside the unpacked folder)" ;;
			esac
			trap - EXIT
			warn "the downloaded, checked file is kept in $tmp"
			exit 1
		fi
	fi

	# The install itself. The packages are not signed yet (the SHA-256 above is the check),
	# so the flags below are the ones an unsigned local package needs.
	case "$TYPE" in
	rpm)
		case "$INSTALLER" in
		dnf | yum) run_root "$INSTALLER" install -y "$tmp/$file" ;; # local files are not gpg-checked by default
		zypper) run_root zypper --non-interactive --no-gpg-checks install "$tmp/$file" ;;
		tdnf) run_root tdnf install -y --nogpgcheck "$tmp/$file" ;;
		esac
		where="/usr (package $PKG)"
		remove="sudo $INSTALLER remove $PKG"
		[ "$INSTALLER" = zypper ] && remove="sudo zypper remove $PKG"
		;;
	deb)
		run_root apt-get install -y "$tmp/$file"
		where="/usr (package $PKG)"
		remove="sudo apt-get remove $PKG"
		;;
	apk)
		run_root apk add --allow-untrusted "$tmp/$file"
		where="/usr (package $PKG)"
		remove="sudo apk del $PKG"
		;;
	tarball)
		if [ "$DRY" = 1 ]; then
			say "+ mkdir <temporary folder>/unpack"
			dir="<temporary folder>/unpack/$PKG-<version>"
		else
			mkdir "$tmp/unpack"
		fi
		if [ "$UNPACK" = zstd ]; then
			say "+ zstd -dc $tmp/$file | tar -xf - -C $tmp/unpack"
			[ "$DRY" = 1 ] || { zstd -dc "$tmp/$file" | tar -xf - -C "$tmp/unpack"; }
		else
			say "+ tar --zstd -xf $tmp/$file -C $tmp/unpack"
			[ "$DRY" = 1 ] || tar --zstd -xf "$tmp/$file" -C "$tmp/unpack"
		fi
		if [ "$DRY" = 0 ]; then
			dir=""
			for d in "$tmp"/unpack/"$PKG"-*; do
				[ -f "$d/install.sh" ] && dir=$d
			done
			[ -n "$dir" ] || die "the tarball has no $PKG-*/install.sh."
		fi
		say "+ cd $dir"
		[ "$DRY" = 1 ] || cd "$dir"
		run_root sh ./install.sh
		where="/usr/local (bin/recipe); the list of files is /usr/local/share/recipe/install-manifest"
		remove="download the same tarball, unpack it, and run 'sudo sh uninstall.sh' in the folder (uninstall.sh is not installed; the manifest lists what it removes)"
		;;
	esac

	say ""
	if [ "$DRY" = 1 ]; then
		say "Dry run finished; nothing was installed. A real run would end with:"
	fi
	say "Installed Recipe ($TYPE) in $where."
	say "Open a new login shell (or run: . /etc/profile.d/recipe.sh), then try: recipe list    recipe install ripgrep"
	say "To remove it: $remove"
}

main "$@"
