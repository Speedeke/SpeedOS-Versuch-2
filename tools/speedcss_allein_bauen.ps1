# speedcss_allein_bauen.ps1 ??? Die Gegenprobe zur Abhaengigkeits-Umkehr
#
# WOZU
# ====
# speedcss hat GENAU EINE erlaubte Abhaengigkeit: speedhtml. CSS ohne
# Dokumentbaum ist bedeutungslos - die Begruendung steht in
# speedcss/Cargo.toml. Verboten bleibt alles andere: kein Kernel, kein
# libspeed, keine Fremdkiste ??? weder auf den Kernel noch auf
# irgendetwas anderes. Das ist die Regel aus docs/browser-v1.md 5,
# und eine Regel ohne Pruefung erodiert.
#
# Dieses Skript prueft zweierlei:
#   1. Der [dependencies]-Block in speedcss/Cargo.toml ist LEER.
#   2. Die Kiste baut ALLEIN ??? fuer das Bare-Metal-Target (wie der Kernel
#      sie benutzt) UND fuer den Host (wie die Tests sie benutzen).
#
# Wer speedcss eine Abhaengigkeit gibt, bricht es.
#
# WICHTIG BEI DIESER KISTE: Sie ist KEIN Mitglied des Kernel-Workspaces
# (siehe Cargo.toml des Kernels) - der Kernel benutzt sie nicht, nur
# userland/htmldump und der kommende Browser. Ein `cargo build` im
# Projektwurzelverzeichnis prueft sie also NICHT mit. Dieses Skript ist
# damit die einzige Stelle, an der ihr Bare-Metal-Bau ueberhaupt geprueft
# wird - es gehoert vor jeden Commit, der sie anfasst.
#
# ACHTUNG: .ps1 in diesem Repo ASCII-only halten (PowerShell 5.1 liest
# UTF-8-ohne-BOM als ANSI).

$ErrorActionPreference = "Stop"
$wurzel = Split-Path -Parent $PSScriptRoot
$kiste  = Join-Path $wurzel "speedcss"

Write-Host "== speedcss: die Gegenprobe ==" -ForegroundColor Cyan

# --- (1) Der Abhaengigkeits-Block muss leer sein ---
$toml = Get-Content (Join-Path $kiste "Cargo.toml")
$imBlock = $false
$gefunden = @()
foreach ($zeile in $toml) {
    $t = $zeile.Trim()
    if ($t -match '^\[.*\]$') { $imBlock = ($t -eq "[dependencies]"); continue }
    if ($imBlock -and $t -ne "" -and -not $t.StartsWith("#")) { $gefunden += $t }
}
$erlaubt = @("speedhtml")
$verboten = @()
foreach ($zeile in $gefunden) {
    $name = ($zeile -split "=")[0].Trim()
    if ($erlaubt -notcontains $name) { $verboten += $zeile }
}
if ($verboten.Count -gt 0) {
    Write-Host "FEHLER: speedcss hat unerlaubte Abhaengigkeiten:" -ForegroundColor Red
    $verboten | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    Write-Host "Erlaubt ist NUR speedhtml (siehe speedcss/Cargo.toml)." -ForegroundColor Red
    exit 1
}
Write-Host "  [ok] [dependencies] enthaelt nur speedhtml." -ForegroundColor Green

# --- (2) Baut sie allein? ---
Push-Location $kiste
try {
    foreach ($ziel in @("x86_64-unknown-none", "x86_64-pc-windows-msvc")) {
        Write-Host "  baue fuer $ziel ..."
        & cargo build --quiet --target $ziel
        if ($LASTEXITCODE -ne 0) {
            Write-Host "FEHLER: Bau fuer $ziel fehlgeschlagen." -ForegroundColor Red
            exit 1
        }
        Write-Host "  [ok] $ziel" -ForegroundColor Green
    }
    Write-Host "  Tests auf dem Host ..."
    & cargo test --quiet --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FEHLER: Tests fehlgeschlagen." -ForegroundColor Red
        exit 1
    }
    Write-Host "  [ok] Tests" -ForegroundColor Green
}
finally {
    Pop-Location
}

Write-Host "speedcss baut und testet ohne Kernel und ohne Abhaengigkeiten." -ForegroundColor Green
