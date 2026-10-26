#!/bin/sh
# LocalFleet installer.
#
#   curl -fsSL https://raw.githubusercontent.com/namastex888/localfleet/dev/install.sh | sh
#   sh install.sh --yes --languages 3b        # from a checkout; options go to `localfleet install`
#
# Gets the code, a pinned uv (checked against locks/uv-releases.json), and runs `localfleet install`
# on uv's own Python 3.12, so the machine needs no Python of its own. It also puts a `localfleet`
# command in ~/.local/bin. Environment: LOCALFLEET_DIR (default ~/.localfleet), LOCALFLEET_REPO,
# LOCALFLEET_BRANCH (default dev).
set -eu

REPO="${LOCALFLEET_REPO:-https://github.com/namastex888/localfleet.git}"
BRANCH="${LOCALFLEET_BRANCH:-dev}"
DIR="${LOCALFLEET_DIR:-$HOME/.localfleet}"
BIN="$HOME/.local/bin"

say() { printf 'localfleet: %s\n' "$*"; }
fail() { say "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "needs $1 ($2)"; }

case "$(uname -s)" in
    Darwin | Linux) ;;
    *) fail "runs on macOS and Linux" ;;
esac
need curl "your package manager has it"
need tar "your package manager has it"
need git "macOS: xcode-select --install; Debian/Ubuntu: sudo apt install git"

# 1. The code: this checkout when run from one, otherwise a clone in $DIR.
here=$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)
if [ -n "$here" ] && [ -f "$here/scripts/localfleet.py" ] && [ -f "$here/locks/uv-releases.json" ]; then
    DIR=$here
elif [ -d "$DIR/.git" ]; then
    say "updating $DIR"
    git -C "$DIR" pull --ff-only -q
else
    say "downloading LocalFleet into $DIR"
    git clone -q --depth 1 --branch "$BRANCH" "$REPO" "$DIR"
fi

# 2. uv: the one on PATH, else the pinned release, verified before use.
if command -v uv >/dev/null 2>&1; then
    UV=$(command -v uv)
else
    UV="$DIR/.tools/uv/uv"
    if [ ! -x "$UV" ]; then
        case "$(uname -m)" in arm64 | aarch64) arch=arm64 ;; *) arch=x86_64 ;; esac
        key="$(uname -s)-$arch"
        block=$(tr -d '\n' < "$DIR/locks/uv-releases.json" | grep -o "\"$key\": *{[^}]*}") ||
            fail "no pinned uv for $key"
        url=$(printf '%s' "$block" | sed 's/.*"url": *"\([^"]*\)".*/\1/')
        sum=$(printf '%s' "$block" | sed 's/.*"sha256": *"\([^"]*\)".*/\1/')
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        say "fetching uv ($key)"
        curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/uv.tar.gz" "$url"
        if command -v sha256sum >/dev/null 2>&1; then
            got=$(sha256sum "$tmp/uv.tar.gz" | cut -d' ' -f1)
        else
            got=$(shasum -a 256 "$tmp/uv.tar.gz" | cut -d' ' -f1)
        fi
        [ "$got" = "$sum" ] || fail "uv download does not match its pinned SHA-256"
        tar -xzf "$tmp/uv.tar.gz" -C "$tmp"
        mkdir -p "$DIR/.tools/uv"
        cp "$(find "$tmp" -type f -name uv | head -n 1)" "$UV"
        chmod 755 "$UV"
    fi
fi

# 3. A `localfleet` command that runs the CLI on uv's Python.
mkdir -p "$BIN"
cat > "$BIN/localfleet" <<EOF
#!/bin/sh
exec "$UV" run --quiet --no-project --python 3.12 "$DIR/scripts/localfleet.py" "\$@"
EOF
chmod 755 "$BIN/localfleet"

# 4. Install. Piped through sh, stdin is this script, so questions read the terminal.
if [ -t 0 ] || ! (: < /dev/tty) 2>/dev/null; then
    "$BIN/localfleet" install "$@"
else
    "$BIN/localfleet" install "$@" < /dev/tty
fi

case ":$PATH:" in
    *":$BIN:"*) ;;
    *) say "add $BIN to your PATH to run 'localfleet' directly" ;;
esac
