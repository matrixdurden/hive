# WSL içinden Windows için derle / çalıştır / kur.
WIN_LOCALAPPDATA := $(shell wslpath "$$(cmd.exe /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')")
INSTALL_DIR      := $(WIN_LOCALAPPDATA)/Programs/cheshire
EXE              := target/x86_64-pc-windows-gnu/release/cheshire.exe

.PHONY: build run debug tree install stop log

build:
	cargo build --release

# Konsollu hata ayıklama sürümü: loglar terminale akar.
debug:
	cargo run

run: build
	./$(EXE)

# Masaüstü pencere ağacını logla (yerleşim sorunlarını ayıklamak için).
tree:
	cargo run -- --agac

# Windows tarafına kopyala. "Windows ile başlat" bu kopyadan çalışmalı,
# yoksa her açılışta WSL'i de uyandırır.
install: build
	-taskkill.exe /IM cheshire.exe /F >/dev/null 2>&1
	mkdir -p "$(INSTALL_DIR)"
	cp $(EXE) "$(INSTALL_DIR)/cheshire.exe"
	@echo "Kuruldu: $(INSTALL_DIR)/cheshire.exe"
	cmd.exe /c start "" "$$(wslpath -w "$(INSTALL_DIR)/cheshire.exe")"

stop:
	-taskkill.exe /IM cheshire.exe /F

log:
	@cat "$$(wslpath "$$(cmd.exe /c 'echo %APPDATA%' 2>/dev/null | tr -d '\r')")/cheshire/cheshire.log"
