# WSL içinden Windows için derle / kur / yayınla.
WIN_LOCALAPPDATA := $(shell wslpath "$$(cmd.exe /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')")
INSTALL_DIR      := $(WIN_LOCALAPPDATA)/Programs/hive
OUT              := target/x86_64-pc-windows-gnu/release
VERSION          := $(shell grep -m1 '^version' app/Cargo.toml | cut -d'"' -f2)

.PHONY: build install stop log ikon release

# hive.exe'nin içine gömülenler önce: soundboard'un mikrofon efekti DLL'i ve duvar kâğıdı motoru.
build:
	cargo build --release -p soundboard-apo
	cargo build --release -p wallpaper
	cargo build --release -p hive

# Windows tarafına kopyala ve başlat (çalışan kopya kapatılır).
install: build
	-taskkill.exe /IM hive.exe /F >/dev/null 2>&1 && sleep 1
	mkdir -p "$(INSTALL_DIR)"
	cp $(OUT)/hive.exe "$(INSTALL_DIR)/hive.exe"
	@echo "Kuruldu: $(INSTALL_DIR)/hive.exe"
	powershell.exe -NoProfile -Command "Start-Process '$$(wslpath -w "$(INSTALL_DIR)/hive.exe")'"

stop:
	-taskkill.exe /IM hive.exe /F

log:
	@cat "$(INSTALL_DIR)/hive.log"

# app/assets/icon-*.png ve icon.ico'yu yeniden çizer.
ikon:
	python3 app/araclar/ikon.py

# GitHub'da v$(VERSION) yayını: hive.exe ve SHA-256'sı. Kurulum komutu (install.ps1) son yayını indirir.
release: build
	@test -z "$$(git status --porcelain)" || { echo "commit edilmemiş değişiklik var"; exit 1; }
	@! git rev-parse -q --verify "refs/tags/v$(VERSION)" >/dev/null || { echo "v$(VERSION) zaten var: app/Cargo.toml sürümünü artır"; exit 1; }
	cd $(OUT) && sha256sum hive.exe | cut -d' ' -f1 > hive.exe.sha256
	gh release create "v$(VERSION)" $(OUT)/hive.exe $(OUT)/hive.exe.sha256 --title "hive $(VERSION)" --generate-notes
