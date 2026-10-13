# WSL içinden Windows için derle / kur / yayınla.
WIN_LOCALAPPDATA := $(shell wslpath "$$(cmd.exe /c 'echo %LOCALAPPDATA%' 2>/dev/null | tr -d '\r')")
INSTALL_DIR      := $(WIN_LOCALAPPDATA)/Programs/winloom
OUT              := target/x86_64-pc-windows-gnu/release
VERSION          := $(shell grep -m1 '^version' Cargo.toml | cut -d'"' -f2)

.PHONY: build install stop log ikon release engram-ses

# winloom.exe'nin içine gömülenler önce: soundboard'un mikrofon efekti DLL'i ve duvar kâğıdı motoru.
build:
	cargo build --release -p soundboard-apo
	cargo build --release -p wallpaper-motor
	cargo build --release -p winloom

# Windows tarafına kopyala ve başlat (çalışan kopya kapatılır).
install: build
	-taskkill.exe /IM winloom.exe /F >/dev/null 2>&1 && sleep 1
	mkdir -p "$(INSTALL_DIR)"
	cp $(OUT)/winloom.exe "$(INSTALL_DIR)/winloom.exe"
	@echo "Kuruldu: $(INSTALL_DIR)/winloom.exe"
	powershell.exe -NoProfile -Command "Start-Process '$$(wslpath -w "$(INSTALL_DIR)/winloom.exe")'"

stop:
	-taskkill.exe /IM winloom.exe /F

log:
	@cat "$(INSTALL_DIR)/winloom.log"

# winloom'ın ve araçların ikonlarını yeniden çizer (merkez/ikon/, araclar/<id>/ikon/).
ikon:
	python3 merkez/ikon/ikon.py
	for f in araclar/*/ikon/ikon.py; do python3 $$f; done

# GitHub'da v$(VERSION) yayını: winloom.exe ve SHA-256'sı. Kurulum komutu (install.ps1) son yayını indirir.
release: build
	@test -z "$$(git status --porcelain)" || { echo "commit edilmemiş değişiklik var"; exit 1; }
	@! git rev-parse -q --verify "refs/tags/v$(VERSION)" >/dev/null || { echo "v$(VERSION) zaten var: Cargo.toml'daki sürümü artır"; exit 1; }
	cd $(OUT) && sha256sum winloom.exe | cut -d' ' -f1 > winloom.exe.sha256
	# Eski adıyla (hive) kurulu sürümler güncellemeyi hive.exe adıyla arar: aynı dosya o adla da yüklenir.
	cp $(OUT)/winloom.exe $(OUT)/hive.exe && cp $(OUT)/winloom.exe.sha256 $(OUT)/hive.exe.sha256
	gh release create "v$(VERSION)" $(OUT)/winloom.exe $(OUT)/winloom.exe.sha256 $(OUT)/hive.exe $(OUT)/hive.exe.sha256 --title "winloom $(VERSION)" --generate-notes

# engram'ın yerel sesi (XTTS v2, ekran kartında): WSL'de ~/.local/share/engram-xtts altına ortam ve sunucu.
# İlk açılışta model (~1,8 GB) Hugging Face'ten iner. Lisans: Coqui Public Model License (ticari değil).
SES_DIR := $(HOME)/.local/share/engram-xtts
engram-ses:
	mkdir -p "$(SES_DIR)"
	test -x "$(SES_DIR)/.venv/bin/python" || uv venv -q --python 3.11 "$(SES_DIR)/.venv"
	uv pip install -q --python "$(SES_DIR)/.venv/bin/python" coqui-tts "transformers>=4.57,<5" speechbrain
	uv pip install -q --python "$(SES_DIR)/.venv/bin/python" "torch==2.7.1" "torchaudio==2.7.1" --index-url https://download.pytorch.org/whl/cu128
	cp araclar/engram/ses/xtts_sunucu.py "$(SES_DIR)/sunucu.py"
	@echo "Yerel ses kuruldu: $(SES_DIR)"
