#!/bin/sh
# Builds rabbithole for every supported platform into dist/, with checksums.txt.
set -eu
cd "$(dirname "$0")"
# The Windows exe icon lives in rsrc_windows_*.syso. To remake it after assets/icon.ico changes:
#   go run github.com/tc-hib/go-winres@v0.3.3 simply --icon assets/icon.ico --arch amd64,arm64 --manifest none --product-name rabbithole --file-description rabbithole

TAGS=with_utls,with_gvisor,badlinkname,tfogo_checklinkname0
VERSION=${VERSION:-$(git describe --tags --always --dirty 2>/dev/null || echo dev)}
LDFLAGS="-s -w -buildid= -checklinkname=0 -X main.version=$VERSION -X runtime.godebugDefault=multipathtcp=0,tlssha1=1"

rm -rf dist
mkdir -p dist
for target in linux/amd64 linux/arm64 windows/amd64 windows/arm64; do
  os=${target%/*}
  arch=${target#*/}
  out=rabbithole-$os-$arch
  [ "$os" = windows ] && out=$out.exe
  CGO_ENABLED=0 GOOS=$os GOARCH=$arch go build -trimpath -tags "$TAGS" -ldflags "$LDFLAGS" -o "dist/$out" .
  echo "dist/$out"
done
# The sing-box app on phones reads the dpi profile from each release.
go run -tags "$TAGS" -ldflags "-checklinkname=0" . __phone-dpi > dist/rabbithole-dpi.json
echo "dist/rabbithole-dpi.json"
(cd dist && sha256sum rabbithole-* > checksums.txt)
echo "dist/checksums.txt ($VERSION)"
