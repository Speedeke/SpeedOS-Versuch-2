# speedpaint_allein_bauen.ps1 - Die Gegenprobe fuer die Mal-Kiste
#
# WOZU
# ====
# speedpaint darf an speedlayout, speedui und speedcss haengen - und an
# SONST NICHTS. Verboten bleibt alles andere: kein Kernel (`speed_os`),
# kein `libspeed`, keine Fremdkiste.
#
# WARUM speedui HIER ERLAUBT IST, obwohl speedlayout es ausdruecklich NICHT
# nehmen durfte: Ein Maler braucht eine ZEICHENFLAECHE, und `Leinwand` ist
# genau diese Abstraktion (zwei Wirte seit Serie 8, Teil 2). Ein eigenes
# Trait daneben waere ein zweiter Name fuer dieselbe Sache. Das Layout
# dagegen braucht nur Textmetrik - drei Zahlen gegen ein ganzes Toolkit.
# Ausfuehrlich in docs/browser-rendern.md 1.
#
# DIE RICHTUNG IST DAS ENTSCHEIDENDE: speedpaint haengt an speedui, nie
# umgekehrt. Dass speedui selbst weiter einen LEEREN [dependencies]-Block
# hat, prueft tools/speedui_allein_bauen.ps1 - beide Skripte gehoeren vor
# jeden Commit, der eine der Kisten anfasst.
#
# Dieses Skript prueft zweierlei:
#   1. Der [dependencies]-Block enthaelt NUR die drei erlaubten Namen.
#   2. Die Kiste baut ALLEIN - fuer das Bare-Metal-Target (so benutzt der
#      Browser sie) UND fuer den Host (so laufen ihre Tests).
#
# WICHTIG BEI DIESER KISTE: Sie ist KEIN Mitglied des Kernel-Workspaces
# (siehe Cargo.toml des Kernels) - der Kernel benutzt sie nicht, nur
# userland/browser. Ein `cargo build` im Projektwurzelverzeichnis prueft
# sie also NICHT mit; dieses Skript ist die einzige Stelle, an der ihr
# Bare-Metal-Bau ueberhaupt geprueft wird.
#
# ACHTUNG: .ps1 in diesem Repo ASCII-only halten (PowerShell 5.1 liest
# UTF-8-ohne-BOM als ANSI).

$ErrorActionPreference = "Stop"
$wurzel = Split-Path -Parent $PSScriptRoot
$kiste  = Join-Path $wurzel "speedpaint"

Write-Host "== speedpaint: die Gegenprobe ==" -ForegroundColor Cyan

# --- (1) Nur die drei erlaubten Abhaengigkeiten ---
$toml = Get-Content (Join-Path $kiste "Cargo.toml")
$imBlock = $false
$gefunden = @()
foreach ($zeile in $toml) {
    $t = $zeile.Trim()
    if ($t -match '^\[.*\]$') { $imBlock = ($t -eq "[dependencies]"); continue }
    if ($imBlock -and $t -ne "" -and -not $t.StartsWith("#")) { $gefunden += $t }
}
$erlaubt = @("speedlayout", "speedui", "speedcss")
$verboten = @()
foreach ($zeile in $gefunden) {
    $name = ($zeile -split "=")[0].Trim()
    if ($erlaubt -notcontains $name) { $verboten += $zeile }
}
if ($verboten.Count -gt 0) {
    Write-Host "FEHLER: speedpaint hat unerlaubte Abhaengigkeiten:" -ForegroundColor Red
    $verboten | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    Write-Host "Erlaubt sind NUR speedlayout, speedui und speedcss." -ForegroundColor Red
    exit 1
}
Write-Host "  [ok] [dependencies] enthaelt nur die drei erlaubten Kisten." -ForegroundColor Green

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

Write-Host "speedpaint baut und testet ohne Kernel." -ForegroundColor Green
