# lyrebird

Sesi sanal mikrofon kurmadan doğrudan gerçek mikrofonun sinyaline ekleyen soundboard motoru (Windows). Discord, oyunlar, OBS, tarayıcı: mikrofonu açan her uygulama duyar.

Arayüzü [hive](../app)'te: motor bir kütüphane olarak onun sürecinde çalışır.

## Nasıl çalışır

Mikrofonun Windows ses zincirine bir efekt (APO, `apo/`) olarak takılır; motor DLL'i içinde taşır ve bağlanırken `Program Files\lyrebird` altına yazar (yönetici ister). Sesler Media Foundation ile çözülür ve `%ProgramData%\lyrebird\bus` ortak belleğine gerçek zamanın biraz önünde yazılır; audiodg.exe içindeki APO oradan okuyup mikrofon sinyaline karıştırır. Gerçek zamanlı yolda bellek ayırma ya da kilit yok.

Zincirdeki yeri:

- Sürücünün akış efekti (SFX) yoksa: SFX'te tek başına, sürücünün tüm işlemesinden sonra. Raw akışlar SFX'i atladığı için uç nokta zincirinde (EFX) bir yedek örnek de durur.
- Sürücünün SFX'i varsa: EFX zincirinin sonu.
- Eski tip sürücülerde EFX yuvasını alır, oradaki sürücü efektini içinde çalıştırır.

Kaldırınca her şey kurulumdan önceki hâline döner. APO'nun logu `%ProgramData%\lyrebird\apo.log`, ses listesi `%APPDATA%\lyrebird\ayarlar.txt`.

| Modül | |
|---|---|
| `bus` | uygulama ile APO arasındaki ortak bellek (APO'ya da derlenir) |
| `player`, `decode` | sesleri çözüp ortak belleğe yazar |
| `monitor` | aynı sesi kulaklığa verir |
| `hotkey` | kısayol biçimi |
| `install` | efekti mikrofonlara takar / çıkarır |
| `probe` | her mikrofona test sesi gönderip geri geliyor mu ölçer |

## Derleme

WSL'den Windows için çapraz derlenir (`x86_64-pc-windows-gnu`, mingw gerekir). DLL önce derlenmeli: motor onu içine gömer.

```sh
cargo build --release -p lyrebird-apo
cargo build --release -p lyrebird
```

hive'da `make build` ikisini sırayla yapar.

## Lisans

MIT
