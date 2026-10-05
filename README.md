<p align="center">
  <img src="docs/banner.png" alt="hive: Windows araçların tek yerde" width="100%">
</p>

<p align="center">
  <a href="https://github.com/matrixdurden/hive/releases/latest"><img src="https://img.shields.io/github/v/release/matrixdurden/hive?style=flat-square&label=sürüm&color=fbbf24&labelColor=16161a" alt="Son sürüm"></a>
  <img src="https://img.shields.io/badge/Windows-10%20%C2%B7%2011-f472b6?style=flat-square&labelColor=16161a" alt="Windows 10 ve 11">
  <img src="https://img.shields.io/badge/Rust-2024-a78bfa?style=flat-square&labelColor=16161a" alt="Rust 2024">
  <a href="LICENSE"><img src="https://img.shields.io/badge/lisans-MIT-e8e8ea?style=flat-square&labelColor=16161a" alt="Lisans: MIT"></a>
</p>

<br>

## Kurulum

PowerShell'e yapıştır:

```powershell
irm https://github.com/matrixdurden/hive/raw/main/install.ps1 | iex
```

Yönetici izni gerekmez. Aynı komut hive'ı günceller.

## Araçlar

hive bir başlatıcı: araçları **Araçlar** sayfasından eklenti gibi kurarsın, kurduğun her araç kenar çubuğunda ve Başlat menüsünde kendi adıyla görünür. "lyrebird" diye aratınca hive o araçta açılır.

| | | |
|:---:|---|---|
| <img src="app/assets/araclar/lyrebird-64.png" width="40"> | **lyrebird** | Sesi doğrudan mikrofona veren soundboard. Sanal mikrofon kurmaz; Discord, oyunlar, OBS, mikrofonu dinleyen her uygulama duyar. |
| <img src="app/assets/araclar/cheshire-64.png" width="40"> | **cheshire** | Masaüstü ikonlarının arkasında GPU ile çizilen canlı duvar kâğıtları. Tam ekranda ve pilde kendiliğinden durur. |
| <img src="app/assets/araclar/rabbithole-64.png" width="40"> | **rabbithole** | Bütün bilgisayarı ağ engellerinin ötesine geçiren tünel: kendi sunucun üzerinden ya da sunucusuz DPI modunda. [Ayrı depo](https://github.com/matrixdurden/rabbithole). |

<table>
  <tr>
    <td width="50%"><img src="docs/araclar.png" alt="Araçlar sayfası"></td>
    <td width="50%"><img src="docs/cheshire.png" alt="cheshire sayfası"></td>
  </tr>
  <tr>
    <td align="center"><sub>Araçlar: kur, aç, kaldır</sub></td>
    <td align="center"><sub>cheshire: duvar kâğıtları ve ayarları</sub></td>
  </tr>
</table>

## İz bırakmaz

Bir aracı kaldırınca kurulurken yaptığı her şey geri alınır: dosyalar, kayıt defteri, mikrofon ayarları, hizmetler, PATH, kısayollar. Kaldırmanın sonunda hive bunları tek tek denetler; bir şey kalmışsa kaldırma başarılı sayılmaz ve neyin kaldığını söyler.

hive'ı kaldırmak (Ayarlar'dan ya da Windows'un **Uygulamalar** listesinden) önce kurulu bütün araçları aynı şekilde kaldırır, sonra kendini.

```powershell
hive --kalinti    # kurulu olmayan araçlardan kalan iz var mı
```

## Derleme

WSL'den Windows için çapraz derlenir (`x86_64-pc-windows-gnu`, mingw gerekir):

```sh
make build      # target/x86_64-pc-windows-gnu/release/hive.exe
make install    # %LOCALAPPDATA%\Programs\hive altına kur ve başlat
make release    # app/Cargo.toml'daki sürümle GitHub'da yayın
```

| Klasör | |
|---|---|
| [`app/`](app) | hive.exe: pencere, araç sayfaları, kurulum ve kaldırma |
| [`lyrebird/`](lyrebird) | lyrebird motoru ve mikrofon efekti (`apo/`) |
| [`cheshire/`](cheshire) | cheshire motoru ve [`.cheshire` formatı](cheshire/README.md#cheshire-formatı) |

## Lisans

MIT
