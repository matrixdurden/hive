# Install winloom:  irm https://github.com/matrixdurden/winloom/raw/main/install.ps1 | iex
#
# Downloads the latest release, checks its SHA-256, installs it under %LOCALAPPDATA%\Programs\winloom
# and starts it. No administrator needed; running it again updates winloom. Nothing is left in %TEMP%.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$tr = (Get-UICulture).TwoLetterISOLanguageName -eq 'tr'
function Say($en, $tr_) { if ($tr) { $tr_ } else { $en } }

$url = 'https://github.com/matrixdurden/winloom/releases/latest/download'
$exe = Join-Path $env:TEMP "winloom-$PID.exe"
$sum = "$exe.sha256"

try {
    Invoke-WebRequest "$url/winloom.exe" -OutFile $exe -UseBasicParsing
    Invoke-WebRequest "$url/winloom.exe.sha256" -OutFile $sum -UseBasicParsing
    $expected = (Get-Content $sum -Raw).Trim().Split()[0]
    if ((Get-FileHash $exe -Algorithm SHA256).Hash -ne $expected) { throw (Say 'the download could not be verified' 'indirilen dosya doğrulanamadı') }

    # -Wait would also wait for the winloom it starts; wait for the installer only.
    $p = Start-Process $exe -ArgumentList '--kur' -PassThru
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw ((Say 'installation failed: ' 'kurulum başarısız: ') + "$env:LOCALAPPDATA\Programs\winloom\kurulum.log") }
    Write-Host (Say 'winloom is installed.' 'winloom kuruldu.') -ForegroundColor Green
}
finally {
    Remove-Item $exe, $sum -ErrorAction SilentlyContinue
}
