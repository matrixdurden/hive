# winloom'a yeni araç eklemek

winloom bir platform: geliştirdiğin her Windows aracı buraya, kendi klasörüne eklenir. Tek
kural: **bir araç = bir klasör.** Aracın sayfası, ikonu ve varsa motoru `araclar/<id>/` altında
durur. Merkezde değişen tek şey araç listesindeki bir satır.

```
winloom/
  merkez/              winloom.exe: pencere, tepsi, araçları açma / kapatma, güncelleme, kurulum
  ortak/               bütün araçların paylaştığı kod
  araclar/
    <id>/
      Cargo.toml
      src/lib.rs       sayfa (ToolPage) ve aracın kendisi
      src/tool.rs      aracın winloom'a tanımı: TOOL
      ikon/            <id>-24/32/40/48/64/128.png ve onları çizen ikon.py
      motor/           (varsa) ayrı süreç ya da kütüphane olarak çalışan motor
```

## İki tür araç

- **Hafif:** winloom'ın içinde çalışır. Pil, Ses aygıtları, Dock, Tıklayıcı ve engram bu
  türden. Sayfa ve iş aynı crate'te durur.
- **Ağır:** işin bir kısmı kendi sürecinde çalışır, winloom onu açar ve kapatır. Duvar kâğıdı
  (`motor/`, winloom.exe'nin içinde taşınır, açılınca diske yazılır) ve VPN (rabbithole, açılınca
  GitHub'dan indirilir) bu türden. Ekran kartı, sistem hizmeti gibi riskli işler buraya girer:
  çökerse winloom'ı ve öbür araçları düşürmez.

**Arayüz her zaman winloom'ın penceresinde.** Araç kendi uygulama penceresini açmaz; ekranları
kenar çubuğundaki sayfasındadır (alt ekranlar varsa sayfanın başlığında sekmeler: engram'ın
"Ana · Hafıza" ve ayarlar düğmesi gibi). Kullanıcı hepsini aynı görür: Araçlar sayfasında bir kart,
aç/kapa anahtarı, kenar çubuğunda bir sayfa. Tepside yalnızca winloom'ın ikonu olur; araçların
tepsi ikonu ve kendi menüsü olmaz. Bildirim gerekiyorsa sayfa `take_notice` ile winloom'ın
tepsisinden gösterir; winloom'ı kendi sayfasında öne getirmek `ortak::hub::post_tab`.

## Adımlar

1. **Kimliği seç.** Küçük harf, tek kelime, İngilizce (`clicker`, `battery`). Klasör adı, veri
   klasörü (`%LOCALAPPDATA%\Programs\winloom\<id>`) ve `winloom --tab <id>` bu olur.
   Görünen ad sonradan değişebilir, kimlik değişmez.

2. **Klasörü aç.** En kolayı en benzer aracı kopyalamak: hafif ve tek dosyalık bir araç için
   `araclar/clicker/`. `Cargo.toml`'da `name`'i kimlik yap. `ortak`, `windows` ve sürüm workspace'ten
   gelir.

3. **Sayfayı yaz** (`src/lib.rs`). Sayfa `ortak::ui::ToolPage`'i uygular: `layout`, `paint`, fare
   ve tuş olayları, `message`. Çizim `ortak::gfx`, düğmeler, anahtarlar ve satırlar `ortak::ui`'da.
   Metinler iki dilli yazılır: `t!("Install", "Kur")`; log için `log!(...)`.

4. **Aracı tanıt** (`src/tool.rs`):

   ```rust
   use ortak::tool::Tool;

   pub static TOOL: Tool = Tool {
       id: "ornek",
       names: ["Example", "Örnek"],
       tagline: ["What it does, in one line", "Ne işe yaradığı, tek cümle"],
       note: ["No internet needed", "İnternet gerekmez"],
       off_note: ["What happens when it is turned off.", "Kapatınca ne olacağı."],
       glyph: "\u{E74C}", // Segoe Fluent Icons: arayüzdeki tek renkli simgesi
       icons: ortak::icons!("ornek"), // Başlat menüsü kısayolunun renkli simgesi
       installed: crate::installed,
       install: crate::install,
       uninstall: crate::uninstall,
       leftovers: crate::leftovers,
       start: |hwnd| Box::new(crate::Ornek::start(hwnd)),
       ..Tool::BASE
   };
   ```

   `lib.rs`'in başına `mod tool; pub use tool::TOOL;` ekle. İsteğe bağlı alanlar (`downloads`,
   `init`, `cli`) `ortak/src/tool.rs`'de açıklanıyor.

   Sayfa isterse iki şey daha verir (`ToolPage`): `summary()` Ana sayfadaki kutucuğunda
   aracın şu anki durumu ("%99" / "Prizde vitesi · şarjda" gibi), `ask(text)` arama kutusuna
   yazılanı araca bırakmak (engram soru olarak alır).

5. **Merkeze bağla.** İki satır:
   - `merkez/Cargo.toml`: `ornek = { path = "../araclar/ornek" }`
   - `merkez/src/tools.rs`: `TOOLS` listesine `&ornek::TOOL` (sıra, kenar çubuğundaki sıra olur).

   Workspace `araclar/*`'ı kendiliğinden görür; kök `Cargo.toml`'a dokunma.

6. **İkonu çiz.** `ikon/ikon.py` aynı koyu yuvarlak kare zeminde tek renkli bir şekil çizer ve
   `<id>-{24,32,40,48,64,128}.png` dosyalarını yazar. Başlamak için `araclar/clicker/ikon/ikon.py`'yi
   kopyala. `make ikon` hepsini yeniden çizer.

7. **Dene.** `make build`, `make install`. Aracı Araçlar sayfasında aç, kullan, kapat. Kapattıktan
   sonra `winloom --leftovers` o araç için "iz yok" demeli.

## Kurallar

- **Açmak ve kapatmak eksiksiz olmalı.** `uninstall` aracın bıraktığı her şeyi (dosya, kayıt
  defteri, zamanlanmış görev, hizmet) siler. `leftovers` aynı izleri listeler ve araç kapalıyken
  boş döner. winloom kapatmayı ancak `leftovers` boşsa başarılı sayar.
- **Bir araç başka bir aracın crate'ine bağlanmaz.** İki aracın ihtiyaç duyduğu şey `ortak/`'a
  taşınır. Kısayol okuma (`ortak::hotkey`) böyle taşındı.
- **Arka plandaki iş arayüzü kilitlemez.** `install` ve `uninstall` zaten arka plan iş
  parçacığında çalışır. Sayfanın kendi uzun işleri de iş parçacığında yapılır, sonucu
  `PostMessageW` ile pencereye gelir.
- **Araca özgü komutlar** (`--ornek-test` gibi) aracın `cli`'sinde yazılır, `main.rs`'e eklenmez.
- **Kimlik aralıkları.** Bütün sayfalar winloom'ın tek penceresini paylaşır. Mesaj, zamanlayıcı ve
  kısayol kimlikleri çakışmasın diye her aracın kendi aralığı var. Yeni araç sıradaki boş aralığı
  alır:

  | Kim | Mesaj (`WM_APP +`) | Zamanlayıcı | Kısayol |
  |---|---|---|---|
  | merkez | 1–9 | | |
  | soundboard | 20–29 | 101– | 1001–5000 |
  | tunnel | 30–39 | 301– | |
  | wallpaper | 40–49 | | |
  | battery | | 401– | |
  | audio | 50–59 | 501– | 5001– |
  | clicker | 60–69 | 601– | 6001– |
  | dock | 70–79 | | |
  | engram | 80–89 | 0x686f, 0x6e74, 0x7663 | (klavye kancası) |
  | **sıradaki araç** | **90–99** | **901–** | **9001–** |

  Aracın kendi açtığı pencerelerin (dock çubuğu gibi) kimlikleri bu tabloya girmez.
