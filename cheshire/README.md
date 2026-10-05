<p align="center">
  <img src="assets/banner.png" alt="cheshire: Windows için hafif, GPU tabanlı canlı duvar kâğıdı" width="100%">
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Windows-f472b6?style=flat-square&labelColor=1c1219" alt="Platform: Windows">
  <img src="https://img.shields.io/badge/GPU-wgpu%20%C2%B7%20DX12-f472b6?style=flat-square&labelColor=1c1219" alt="GPU: wgpu, DX12">
  <img src="https://img.shields.io/badge/shader-GLSL%20(Shadertoy)-f472b6?style=flat-square&labelColor=1c1219" alt="Shader: GLSL, Shadertoy lehçesi">
  <img src="https://img.shields.io/badge/Rust-2024-f472b6?style=flat-square&labelColor=1c1219" alt="Rust 2024">
  <a href="../LICENSE"><img src="https://img.shields.io/badge/lisans-MIT-f472b6?style=flat-square&labelColor=1c1219" alt="Lisans: MIT"></a>
</p>

<p align="center">
  <a href="#kullanım">Kullanım</a> ·
  <a href="#cheshire-formatı"><code>.cheshire</code> formatı</a> ·
  <a href="#derleme">Derleme</a>
</p>

<br>

Masaüstü ikonlarının arkasında Shadertoy lehçesinde GLSL shader'lar çizer. Tam ekran bir pencere onu kapattığında ya da (istersen) pil ile çalışırken durur.

<table>
  <tr>
    <td width="33%"><img src="assets/akis.jpg" alt="Akış duvar kâğıdı"></td>
    <td width="33%"><img src="assets/nabiz.jpg" alt="Nabız duvar kâğıdı"></td>
    <td width="33%"><img src="assets/domore.jpg" alt="Do more duvar kâğıdı"></td>
  </tr>
  <tr>
    <td align="center"><b>Akış</b><br><sub>Kıvrılan bir alanda akan renkli lifler. Fare lifleri dışa iter.</sub></td>
    <td align="center"><b>Nabız</b><br><sub>Çalan müziğe tepki veren halka.</sub></td>
    <td align="center"><b>Do more</b><br><sub>Kağıt kesimi katmanlar üstünde saat ve slogan, 5 renk teması. Yazı gömülü Montserrat konturlarıyla.</sub></td>
  </tr>
</table>

<p align="center"><sub>Hepsi <a href="ornekler"><code>ornekler/</code></a> içinde. Görseller <code>cheshire --onizleme</code> ile üretildi.</sub></p>

## Kullanım

cheshire [hive](../README.md)'ın parçası: hive'ı kur, **Araçlar** sayfasından cheshire'ı ekle. Duvar kâğıdını ve ayarlarını hive'daki cheshire sayfasından seçersin; bir `.cheshire` dosyasını pencereye sürüklemek onu ekler ve uygular. Motor (`cheshire.exe --hub`) hive'ın içinde gelir, ayrı bir süreç olarak çalışır.

Her şey tek klasörde durur: `%LOCALAPPDATA%\Programs\cheshire` (exe, `cheshire.ini`, `duvarlar\`, log). hive'dan kaldırınca klasör, önizlemeler ve kayıtlar silinir, duvar kâğıdı Windows'unkine döner.

Komut satırı araçları:

```sh
cheshire --dogrula dosya.cheshire               # derle, 120 kare çiz, GPU süresini raporla
cheshire --onizleme dosya.cheshire cikti.png    # PNG üret
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

| Girdi | |
| --- | --- |
| `iTime`, `iResolution`, `iMouse` | Shadertoy'daki gibi |
| `iChannel0..3` | buffer'lar (`buf A`..`buf D`) |
| `iAudio` | müziğe tepki için ses verisi |

Örnekler [`ornekler/`](ornekler) içinde.

## Derleme

hive'ın deposunun kökünden: `make build` motoru da derler ve hive.exe'nin içine gömer (`target/x86_64-pc-windows-gnu/release/cheshire.exe`).
