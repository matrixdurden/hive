# hive kurulumu:  irm https://github.com/matrixdurden/hive/raw/main/install.ps1 | iex
#
# Son sürümü indirir, SHA-256'sını doğrular, %LOCALAPPDATA%\Programs\hive altına kurar ve başlatır.
# Yönetici izni gerekmez. Yeniden çalıştırmak hive'ı günceller. İndirilen geçici dosya silinir.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$url = 'https://github.com/matrixdurden/hive/releases/latest/download'
$exe = Join-Path $env:TEMP "hive-$PID.exe"
$sum = "$exe.sha256"

try {
    Invoke-WebRequest "$url/hive.exe" -OutFile $exe -UseBasicParsing
    Invoke-WebRequest "$url/hive.exe.sha256" -OutFile $sum -UseBasicParsing
    $expected = (Get-Content $sum -Raw).Trim().Split()[0]
    if ((Get-FileHash $exe -Algorithm SHA256).Hash -ne $expected) { throw 'indirilen dosya doğrulanamadı' }

    # -Wait başlattığı hive'ı da beklerdi; yalnızca kurulum sürecini bekle.
    $p = Start-Process $exe -ArgumentList '--kur' -PassThru
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw "kurulum başarısız: $env:LOCALAPPDATA\Programs\hive\kurulum.log" }
    Write-Host 'hive kuruldu.' -ForegroundColor Green
}
finally {
    Remove-Item $exe, $sum -ErrorAction SilentlyContinue
}
