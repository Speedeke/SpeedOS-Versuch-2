# testseiten_holen.ps1 -- Die echten Webseiten fuer die Parser-Tests holen
#
# WOZU
# ====
# `speedhtml` wird gegen ECHTE Seiten getestet, nicht nur gegen selbst
# ausgedachte Schnipsel. Selbst ausgedachtes HTML hat die Eigenschaft, genau
# die Fehler zu enthalten, an die man beim Schreiben gedacht hat -- und
# genau die anderen nicht.
#
# Die Seiten liegen im Repository (assets/testseiten/) und werden NICHT beim
# Testen geholt. Zwei Gruende, beide aus der Netz-Testmethodik von Serie 5:
#   1. Eine Testsuite darf nicht von fremden Servern abhaengen.
#   2. Echte Seiten aendern sich. Ein Test, dessen Eingabe sich unter der
#      Hand aendert, ist kein Test.
#
# Deshalb wird von HAND geholt -- mit diesem Skript, bewusst und mit Datum.
# Dasselbe Prinzip wie beim CA-Buendel (tools/ca_bundle_holen.ps1): Was von
# aussen ins Repository kommt, kommt sichtbar.
#
# WANN AUSFUEHREN: Praktisch nie. Nur, wenn eine Testseite fehlt oder man
# bewusst eine neuere Fassung will -- und dann gehoert der Testlauf danach
# angesehen, denn eine neue Fassung kann andere Zahlen ergeben.
#
# ACHTUNG: .ps1 in diesem Repo ASCII-only halten (PowerShell 5.1 liest
# UTF-8-ohne-BOM als ANSI).

$ErrorActionPreference = "Stop"
$wurzel = Split-Path -Parent $PSScriptRoot
$ziel = Join-Path $wurzel "assets\testseiten"

# Name, URL, Zweck
$seiten = @(
    @{
        Datei = "cern-theproject.html"
        Url   = "http://info.cern.ch/hypertext/WWW/TheProject.html"
        Zweck = "Pruefseite A: die erste Webseite der Welt (1991). Reines HTML."
    },
    @{
        Datei = "wikipedia-betriebssystem.html"
        Url   = "https://de.wikipedia.org/wiki/Betriebssystem"
        Zweck = "Pruefseite B: Tabellen, Bilder, Skripte, Formular."
    },
    @{
        Datei = "example.com.html"
        Url   = "https://example.com"
        Zweck = "Kontrollfall: die kleinste gueltige Seite."
    }
)

if (-not (Test-Path $ziel)) { New-Item -ItemType Directory $ziel | Out-Null }

Write-Host "== Testseiten holen ==" -ForegroundColor Cyan
$zeilen = New-Object System.Collections.Generic.List[string]
$zeilen.Add("Herkunft der Testseiten")
$zeilen.Add("=======================")
$zeilen.Add("")
$zeilen.Add("Geholt mit tools/testseiten_holen.ps1 am " + (Get-Date -Format "yyyy-MM-dd"))
$zeilen.Add("")
$zeilen.Add("Diese Dateien sind ECHTE, HERUNTERGELADENE Webseiten und dienen als")
$zeilen.Add("Testeingabe fuer speedhtml (Host-Tests, include_str!). Sie landen NIE")
$zeilen.Add("im SpeedOS-Image -- Ausnahme: cern-theproject.html wird zusaetzlich")
$zeilen.Add("eingebettet, damit htmldump auch ohne Netz etwas zu tun hat.")
$zeilen.Add("")
$zeilen.Add("URHEBERRECHT: Die Inhalte gehoeren ihren Urhebern und liegen hier")
$zeilen.Add("unveraendert als Testeingabe fuer einen Parser. Der Wikipedia-Artikel")
$zeilen.Add("steht unter CC BY-SA 4.0.")
$zeilen.Add("")

foreach ($seite in $seiten) {
    $pfad = Join-Path $ziel $seite.Datei
    Write-Host ("  hole " + $seite.Url + " ...")
    try {
        Invoke-WebRequest -Uri $seite.Url -OutFile $pfad -UseBasicParsing `
            -UserAgent "SpeedOS-Testseiten/1.0" -TimeoutSec 40
    }
    catch {
        Write-Host ("  FEHLER: " + $_.Exception.Message) -ForegroundColor Red
        Write-Host "  (die vorhandene Datei bleibt unangetastet)" -ForegroundColor Yellow
        continue
    }
    $bytes = [System.IO.File]::ReadAllBytes($pfad)
    $hash = (Get-FileHash -Path $pfad -Algorithm SHA256).Hash.ToLower()
    Write-Host ("  [ok] " + $seite.Datei + "  " + $bytes.Length + " Byte") -ForegroundColor Green

    $zeilen.Add($seite.Datei)
    $zeilen.Add("  Quelle:  " + $seite.Url)
    $zeilen.Add("  Groesse: " + $bytes.Length + " Byte")
    $zeilen.Add("  SHA-256: " + $hash)
    $zeilen.Add("  Zweck:   " + $seite.Zweck)
    $zeilen.Add("")
}

$herkunft = Join-Path $ziel "HERKUNFT.txt"
[System.IO.File]::WriteAllLines($herkunft, $zeilen)
Write-Host ("Herkunft vermerkt in " + $herkunft) -ForegroundColor Green
Write-Host ""
Write-Host "ACHTUNG: Die Tests in speedhtml/src/tests.rs pruefen Inhalte dieser" -ForegroundColor Yellow
Write-Host "Seiten (Ueberschriften, Linkzahlen, Tabellen). Nach einem Neu-Holen" -ForegroundColor Yellow
Write-Host "also einmal ausfuehren:" -ForegroundColor Yellow
Write-Host "  powershell -File tools\speedhtml_allein_bauen.ps1"
