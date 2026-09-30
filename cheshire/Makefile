# WSL içinden Windows için derle / çalıştır / kur / yayınla.
WIN_LOCALAPPDATA := $(shell wslpath "$$(cmd.exe /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')")
APP_DIR          := $(WIN_LOCALAPPDATA)/Programs/cheshire
EXE              := target/x86_64-pc-windows-gnu/release/cheshire.exe
VERSION          := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

.PHONY: build run debug tree install stop log release

build:
	cargo build --release

# Konsollu hata ayıklama sürümü: loglar terminale akar.
debug:
	cargo run

# Yerinde (WSL'den) çalıştır: kurmaz, kayıt yapmaz.
run: build
	./$(EXE)

# Masaüstü pencere ağacını logla (yerleşim sorunlarını ayıklamak için).
tree:
	cargo run -- --agac

# Bu derlemeyi Windows'a kur: çalışan kopyayı kapatır, %LOCALAPPDATA%\Programs\cheshire'a
# kopyalar ve oradan başlatır. Kullanıcı aynı şeyi indirdiği exe'ye çift tıklayarak yapar.
install: build
	./$(EXE) --kur

stop:
	-taskkill.exe /IM cheshire.exe /F

log:
	@cat "$(APP_DIR)/cheshire.log"

# Cargo.toml'daki sürümle GitHub'da yayın açar; kurulu kopyalar bunu otomatik güncelleme ile alır.
# Önce sürümü artır, commit'le ve push'la.
release:
	@test -z "$$(git status --porcelain)" || { echo "Çalışma ağacı temiz değil."; exit 1; }
	@git fetch -q origin && test "$$(git rev-parse HEAD)" = "$$(git rev-parse origin/main)" \
		|| { echo "HEAD origin/main ile aynı değil: önce push'la."; exit 1; }
	@! gh release view v$(VERSION) >/dev/null 2>&1 || { echo "v$(VERSION) zaten var: Cargo.toml'da sürümü artır."; exit 1; }
	cargo build --release
	cd $(dir $(EXE)) && sha256sum cheshire.exe > cheshire.exe.sha256
	gh release create v$(VERSION) $(EXE) $(EXE).sha256 --title "cheshire $(VERSION)" --generate-notes
