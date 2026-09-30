# cheshire

Windows için hafif, GPU tabanlı canlı duvar kâğıdı motoru. Masaüstü ikonlarının arkasında Shadertoy lehçesinde GLSL shader'lar çizer. Tam ekran bir pencere onu kapattığında ya da (istersen) pil ile çalışırken durur.

## Kullanım

- `cheshire.exe`'yi çalıştır. Sistem tepsisindeki simgeden duvar kâğıdını seç, parametreleri ayarla, "Windows ile başlat"ı aç.
- Bir `.cheshire` dosyasına çift tıkla ya da exe'nin üstüne sürükle: kurulur ve uygulanır.
- Ayarlar, log ve duvar kâğıtları `%APPDATA%\cheshire` altında durur.

```
cheshire --dogrula dosya.cheshire               derle, 120 kare çiz, GPU süresini raporla
cheshire --onizleme dosya.cheshire cikti.png    PNG üret
```

## `.cheshire` formatı

Metadata yorum satırlarında, gövde Shadertoy'daki gibi `mainImage`:

```glsl
// @cheshire
// ad: Akış
// fps: 60
// param hiz: 1.0 [0.2, 3]
// param renk: #ff6ec7
//--- ortak      (isteğe bağlı, her geçişin başına eklenir)
//--- buf A      (isteğe bağlı, A..D)
//--- image      (bölüm yoksa tüm gövde image sayılır)
```

`iTime`, `iResolution`, `iMouse`, `iChannel0..3` (buffer'lar) ve müziğe tepki için `iAudio` kullanılabilir. Örnekler [`ornekler/`](ornekler) içinde.

## Derleme

WSL'den Windows için çapraz derlenir (`x86_64-pc-windows-gnu`, mingw gerekir):

```sh
make build     # target/x86_64-pc-windows-gnu/release/cheshire.exe
make install   # %LOCALAPPDATA%\Programs\cheshire altına kopyala ve başlat
```

## Lisans

MIT
