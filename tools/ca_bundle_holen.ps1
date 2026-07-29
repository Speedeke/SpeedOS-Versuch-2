# ca_bundle_holen.ps1 - holt das CA-Buendel fuer SpeedOS (Serie 7, Teil 2)
#
# WOZU: Wurzelzertifikate sind der Vertrauensanker des ganzen Systems.
# Sie werden BEWUSST und EINMALIG geholt, nicht im Hintergrund von einem
# Build-Skript - ein Anker, der unbemerkt entsteht, ist wertlos.
# Die Begruendung steht vollstaendig in docs/tls-vertrauen.md.
#
# QUELLE: https://curl.se/ca/cacert.pem
#   Der von curl gepflegte Export von Mozillas NSS-Wurzelspeicher. Eine
#   Datei, ein Format, taeglich gebaut, mit Datum und Pruefsumme
#   veroeffentlicht.
#
# ERGEBNIS:
#   assets/ca-bundle.pem            das Buendel
#   assets/ca-bundle.herkunft.txt   URL, Datum, Groesse, SHA-256
#
# Das build.rs bettet die PEM-Datei ins Kernel-Image ein; der Kernel legt
# sie beim Boot nach /platte/system/ca-bundle.pem. Fehlt sie, baut SpeedOS
# trotzdem - dann eben ohne Vertrauensanker, mit deutlicher Meldung.
#
# ACHTUNG: Diese Datei ist ASCII-only (Projektregel - PowerShell 5.1 liest
# UTF-8-ohne-BOM als ANSI, Umlaute zerlegen den Parser).

[CmdletBinding()]
param(
    # Andere Quelle (z. B. eine lokale Kopie oder ein Spiegel).
    [string]$Url = "https://curl.se/ca/cacert.pem",
    # Nur pruefen, was da ist - nichts holen.
    [switch]$NurPruefen
)

$ErrorActionPreference = "Stop"

$wurzel  = Split-Path -Parent $PSScriptRoot
$assets  = Join-Path $wurzel "assets"
$pem     = Join-Path $assets "ca-bundle.pem"
$herkunft= Join-Path $assets "ca-bundle.herkunft.txt"

function Zeige-Bestand {
    if (-not (Test-Path $pem)) {
        Write-Host "Kein Buendel vorhanden ($pem)." -ForegroundColor Yellow
        return $false
    }
    $groesse = (Get-Item $pem).Length
    $anzahl  = (Select-String -Path $pem -Pattern "BEGIN CERTIFICATE" -SimpleMatch).Count
    Write-Host "Vorhanden: $pem"
    Write-Host "  $groesse Byte, $anzahl Zertifikat(e)"
    if (Test-Path $herkunft) {
        Write-Host "--- Herkunft ---"
        Get-Content $herkunft | ForEach-Object { Write-Host "  $_" }
    } else {
        Write-Host "  (keine Herkunfts-Notiz - Datei von Hand abgelegt?)" -ForegroundColor Yellow
    }
    return $true
}

if ($NurPruefen) {
    Zeige-Bestand | Out-Null
    exit 0
}

if (-not (Test-Path $assets)) {
    New-Item -ItemType Directory -Path $assets | Out-Null
}

Write-Host "Hole CA-Buendel von $Url ..."
$temp = Join-Path $env:TEMP ("speedos-ca-" + [guid]::NewGuid().ToString() + ".pem")
try {
    # TLS 1.2 erzwingen - PowerShell 5.1 verhandelt sonst je nach
    # Windows-Version noch SSL3/TLS1.0.
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $Url -OutFile $temp -UseBasicParsing
} catch {
    Write-Host "FEHLGESCHLAGEN: $($_.Exception.Message)" -ForegroundColor Red
    Write-Host "SpeedOS baut auch ohne Buendel - dann aber ohne Vertrauensanker."
    exit 1
}

# PLAUSIBILITAET, bevor irgendetwas ins Repository wandert. Eine
# Fehlerseite eines Proxys ist auch eine Datei - sie ist nur keine.
$inhalt = Get-Content $temp -Raw
$anzahl = ([regex]::Matches($inhalt, "-----BEGIN CERTIFICATE-----")).Count
$enden  = ([regex]::Matches($inhalt, "-----END CERTIFICATE-----")).Count
if ($anzahl -lt 50 -or $anzahl -ne $enden) {
    Write-Host "ABGELEHNT: $anzahl BEGIN- und $enden END-Marken gefunden." -ForegroundColor Red
    Write-Host "Das sieht nicht nach einem CA-Buendel aus (erwartet: > 50, paarweise)."
    Remove-Item $temp -Force
    exit 1
}

$hash    = (Get-FileHash -Path $temp -Algorithm SHA256).Hash
$groesse = (Get-Item $temp).Length
$datum   = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd HH:mm:ss 'UTC'")

Move-Item -Path $temp -Destination $pem -Force

@(
    "SpeedOS CA-Buendel - Herkunft"
    "============================="
    "URL:        $Url"
    "Abgerufen:  $datum"
    "Groesse:    $groesse Byte"
    "SHA-256:    $hash"
    "Zertifikate: $anzahl"
    ""
    "Geholt mit tools/ca_bundle_holen.ps1. Aktualisierung erfolgt VON HAND -"
    "siehe docs/tls-vertrauen.md, Abschnitt 2 (inklusive der Begruendung,"
    "warum es keine automatische Aktualisierung gibt)."
) | Set-Content -Path $herkunft -Encoding utf8

Write-Host ""
Write-Host "Fertig." -ForegroundColor Green
Zeige-Bestand | Out-Null
Write-Host ""
Write-Host "Naechster Schritt: cargo build (das Buendel wird eingebettet und"
Write-Host "beim Boot nach /platte/system/ca-bundle.pem geschrieben)."
