#!/bin/sh
# Print the Homebrew formula for a bogyman release.
#
#   scripts/homebrew-formula.sh 0.2.0 SHA256SUMS > Formula/bogyman.rb
#
# SHA256SUMS is the file the release workflow publishes next to the tarballs.
# The release workflow runs this to update soleo/homebrew-bogyman; it also
# works by hand after downloading SHA256SUMS from a release.
set -eu

version=${1:?usage: homebrew-formula.sh <version> <SHA256SUMS>}
sums=${2:?usage: homebrew-formula.sh <version> <SHA256SUMS>}
version=${version#v}
base="https://github.com/soleo/bogyman-cli/releases/download/v$version"

sha() {
  file="bogyman-v$version-$1.tar.gz"
  sum=$(awk -v f="$file" '$2 == f { print $1 }' "$sums")
  if [ -z "$sum" ]; then
    echo "homebrew-formula.sh: $file is not in $sums" >&2
    exit 1
  fi
  echo "$sum"
}

mac_arm=$(sha aarch64-apple-darwin)
mac_intel=$(sha x86_64-apple-darwin)
linux_arm=$(sha aarch64-unknown-linux-gnu)
linux_intel=$(sha x86_64-unknown-linux-gnu)

cat <<FORMULA
class Bogyman < Formula
  desc "Which of your agent sessions needs you, on what, and for how long"
  homepage "https://github.com/soleo/bogyman-cli"
  version "$version"
  license "MIT"

  on_macos do
    on_arm do
      url "$base/bogyman-v$version-aarch64-apple-darwin.tar.gz"
      sha256 "$mac_arm"
    end
    on_intel do
      url "$base/bogyman-v$version-x86_64-apple-darwin.tar.gz"
      sha256 "$mac_intel"
    end
  end

  on_linux do
    on_arm do
      url "$base/bogyman-v$version-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$linux_arm"
    end
    on_intel do
      url "$base/bogyman-v$version-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$linux_intel"
    end
  end

  def install
    bin.install "bogyman"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/bogyman --version")
  end
end
FORMULA
