# tools/usb_schreiben.ps1 -- Schreibt speedos-live.img ROH auf einen USB-Stick
#
# ACHTUNG: Das LOESCHT den Ziel-USB-Stick vollstaendig und unwiderruflich.
# Deshalb mit mehreren Sicherungen:
#   - laeuft NUR als Administrator (Rohzugriff auf die Platte),
#   - zeigt NUR USB-/Wechsel-Platten zur Auswahl (BusType USB, keine
#     System-/Boot-Platte, < 512 GB) -- die interne NVMe steht gar nicht
#     erst auf der Liste und kann nicht getroffen werden,
#   - laesst den Stick per NUMMER waehlen, wenn mehrere stecken,
#   - zeigt danach Modell, Groesse UND die Laufwerksbuchstaben mit ihren
#     Datentraegernamen -- daran erkennt man den Stick wieder,
#   - verlangt ZWEI Bestaetigungen: die Nummer und getipptes LOESCHEN.
#
# WARUM DIE AUSWAHL UND NICHT DIE AUTOMATIK: Frueher nahm das Skript
# stillschweigend den einzigen Stick und brach bei mehreren ab. Wer zwei
# Sticks steckt (der eine mit Sicherungen), musste die Disk-Nummer von
# Hand heraussuchen -- genau die Situation, in der man sich vertippt.
# Jetzt steht die Liste da, und die Wahl ist ein bewusster Tastendruck.
#
# Aufruf: Rechtsklick auf die Datei -> "Mit PowerShell ausfuehren" schlaegt
# fehl (keine Admin-Rechte). Stattdessen:
#   1. Startmenue -> "PowerShell" -> Rechtsklick -> "Als Administrator
#      ausfuehren".
#   2. In das Projektverzeichnis wechseln, dann:  .\tools\usb_schreiben.ps1
#   (bei Bedarf einmalig:  Set-ExecutionPolicy -Scope Process Bypass )
#
# ASCII-only mit Absicht (PowerShell 5.1 liest .ps1 ohne BOM als ANSI).

param(
    [string]$Image = "speedos-live.img",
    [int]$DiskNummer = -1,         # -1 = interaktiv auswaehlen
    [switch]$Ja                    # Bestaetigungen ueberspringen (Skript-Betrieb)
)

$ErrorActionPreference = "Stop"

# --- 1. Administrator-Rechte pruefen ---
$pr = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $pr.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host "FEHLER: Dieses Skript braucht Administrator-Rechte (Rohzugriff auf die Platte)." -ForegroundColor Red
    Write-Host "  -> PowerShell 'Als Administrator ausfuehren' und erneut starten." -ForegroundColor Yellow
    exit 1
}

# --- 2. Image finden ---
if (-not (Test-Path $Image)) { throw "Image nicht gefunden: $Image  (zuerst 'cargo image' ausfuehren)" }
$imgVoll = (Resolve-Path $Image).Path
$imgLen  = (Get-Item $imgVoll).Length
Write-Host "Image: $imgVoll ($([math]::Round($imgLen/1MB,1)) MiB)"

# --- 3. Ziel-Datentraeger bestimmen (mit strengen Sicherungen) ---
$kandidaten = @(Get-Disk | Where-Object {
    $_.BusType -eq 'USB' -and -not $_.IsSystem -and -not $_.IsBoot -and $_.Size -lt 512GB
} | Sort-Object Number)

if ($kandidaten.Count -eq 0) {
    Write-Host "Keine USB-/Wechselplatte gefunden." -ForegroundColor Red
    Write-Host "  -> Stick eingesteckt? Kurz abziehen und neu einstecken, dann erneut starten." -ForegroundColor Yellow
    exit 1
}

# Eine Zeile je Stick, mit allem, woran man ihn wiedererkennt: Modell,
# Groesse und die Laufwerksbuchstaben MIT Datentraegernamen. Nur die
# Nummer waere eine Zahl ohne Bedeutung -- der Name auf dem Stick ist
# das, was man im Explorer sieht.
function Stick-Beschriftung($nummer) {
    $teile = @()
    try {
        Get-Partition -DiskNumber $nummer -ErrorAction Stop |
            Where-Object DriveLetter |
            ForEach-Object {
                $vol = Get-Volume -DriveLetter $_.DriveLetter -ErrorAction SilentlyContinue
                if ($vol -and $vol.FileSystemLabel) {
                    $teile += ("{0}: '{1}'" -f $_.DriveLetter, $vol.FileSystemLabel)
                } else {
                    $teile += ("{0}:" -f $_.DriveLetter)
                }
            }
    } catch {}
    if ($teile.Count -eq 0) { return "(keine Partition sichtbar)" }
    return ($teile -join "  ")
}

Write-Host ""
Write-Host "Gefundene USB-Datentraeger:" -ForegroundColor Cyan
Write-Host ""
for ($i = 0; $i -lt $kandidaten.Count; $i++) {
    $k = $kandidaten[$i]
    Write-Host ("  [{0}]  Disk {1}  {2,-26} {3,6} GB   {4}" -f `
        ($i + 1), $k.Number, $k.FriendlyName, [math]::Round($k.Size/1GB,1), (Stick-Beschriftung $k.Number))
}
Write-Host ""
Write-Host "  Die interne Festplatte steht hier NICHT und kann nicht getroffen werden." -ForegroundColor DarkGray
Write-Host ""

if ($DiskNummer -ge 0) {
    # Ausdruecklich vorgegeben (Skript-Betrieb) -- trotzdem durch die
    # Sicherungs-Wall unten.
    $ziel = Get-Disk -Number $DiskNummer
} elseif ($kandidaten.Count -eq 1 -and $Ja) {
    $ziel = $kandidaten[0]
} else {
    # ERSTE BESTAETIGUNG: die Auswahl. Auch bei genau EINEM Stick wird
    # gefragt -- "es ist ja nur einer" ist genau die Annahme, die beim
    # zweiten Stick teuer wird.
    $eingabe = Read-Host ("Welcher Stick? 1-{0} eintippen (leer = abbrechen)" -f $kandidaten.Count)
    if ([string]::IsNullOrWhiteSpace($eingabe)) {
        Write-Host "Abgebrochen -- nichts veraendert."
        exit 0
    }
    $wahl = 0
    if (-not [int]::TryParse($eingabe.Trim(), [ref]$wahl) -or $wahl -lt 1 -or $wahl -gt $kandidaten.Count) {
        Write-Host "Ungueltige Eingabe -- nichts veraendert." -ForegroundColor Red
        exit 1
    }
    $ziel = $kandidaten[$wahl - 1]
}

# Sicherungs-Wall: das gewaehlte Ziel MUSS ein USB-Wechsel-Medium sein.
if ($ziel.BusType -ne 'USB' -or $ziel.IsSystem -or $ziel.IsBoot -or $ziel.Size -ge 512GB) {
    Write-Host "ABBRUCH: Datentraeger $($ziel.Number) ist KEINE sichere USB-Zielplatte" -ForegroundColor Red
    Write-Host "  (BusType=$($ziel.BusType), System=$($ziel.IsSystem), Boot=$($ziel.IsBoot), $([math]::Round($ziel.Size/1GB,1)) GB)." -ForegroundColor Red
    exit 1
}

$gb = [math]::Round($ziel.Size/1GB,1)
Write-Host ""
Write-Host "==================== ZIEL-USB-STICK ====================" -ForegroundColor Cyan
Write-Host ("  Datentraeger : {0}" -f $ziel.Number)
Write-Host ("  Modell       : {0}" -f $ziel.FriendlyName)
Write-Host ("  Groesse      : {0} GB   (BusType {1})" -f $gb, $ziel.BusType)
Write-Host ("  Laufwerke    : {0}" -f (Stick-Beschriftung $ziel.Number))
Write-Host "========================================================" -ForegroundColor Cyan
Write-Host "ALLE DATEN auf diesem Stick werden GELOESCHT." -ForegroundColor Red

# ZWEITE BESTAETIGUNG. `-Ja` ueberspringt sie fuer den Skript-Betrieb --
# von Hand sollte man sie NICHT umgehen: Ein getipptes Wort ist die
# letzte Gelegenheit, den falschen Stick zu bemerken.
if (-not $Ja) {
    $antwort = Read-Host "Zum Fortfahren  LOESCHEN  eintippen (alles andere bricht ab)"
    if ($antwort -ne "LOESCHEN") { Write-Host "Abgebrochen -- nichts veraendert."; exit 0 }
} else {
    Write-Host "(-Ja gesetzt: Bestaetigung uebersprungen)" -ForegroundColor Yellow
}

# --- 4. Partitionen entfernen (haengt die Volumes aus) ---
# WICHTIG: KEIN Set-Disk -IsOffline -- Wechselmedien (USB-Sticks) koennen
# gar nicht offline gesetzt werden ("Removable media cannot be set to
# offline"). Nach Clear-Disk ist der Stick partitionslos, also kann der
# Rohschreib-Zugriff darauf trotzdem gelingen, obwohl er online bleibt.
Write-Host "Bereite Datentraeger vor ..."
Clear-Disk -Number $ziel.Number -RemoveData -RemoveOEM -Confirm:$false
# Schreibschutz best-effort loesen (manche Sticks melden ihn); Fehler
# hier sind unkritisch:
Set-Disk -Number $ziel.Number -IsReadOnly $false -ErrorAction SilentlyContinue

# --- 4b. Warten bis Windows den Stick nach Clear-Disk fertig ---
#         re-enumeriert hat (Plug&Play). Ohne diese Pause kann der
#         PhysicalDrive-Handle ungueltig werden ("Ein nicht vorhandenes
#         Geraet wurde angegeben").
$diskNr      = $ziel.Number
$maxWarte    = 15          # Sekunden Obergrenze
$pruefPause  = 1           # Sekunden zwischen den Pruefungen
$gewartet    = 0
Write-Host "Warte auf Geraete-Stabilisierung ..." -NoNewline
while ($gewartet -lt $maxWarte) {
    Start-Sleep -Seconds $pruefPause
    $gewartet += $pruefPause
    try {
        # Update-Disk zwingt Windows, den Datentraeger jetzt einzulesen.
        Update-Disk -Number $diskNr -ErrorAction Stop
        # Pruefe, ob der Datentraeger noch da ist und ansprechbar:
        $check = Get-Disk -Number $diskNr -ErrorAction Stop
        if ($check -and $check.BusType -eq 'USB') {
            # Kurzer Test-Open, um sicherzugehen, dass der Handle klappt:
            $testHnd = $null
            try {
                $testHnd = [System.IO.File]::Open(
                    "\\.\PhysicalDrive$diskNr", 'Open', 'Write', 'ReadWrite')
                # Handle laesst sich oeffnen -- Geraet ist stabil.
                break
            } catch {
                # Noch nicht bereit, naechste Runde.
            } finally {
                if ($testHnd) { $testHnd.Close() }
            }
        }
    } catch {
        # Get-Disk / Update-Disk schlagen noch fehl -- weiter warten.
    }
    Write-Host "." -NoNewline
}
Write-Host ""
if ($gewartet -ge $maxWarte) {
    Write-Host "WARNUNG: Datentraeger reagiert nach ${maxWarte}s noch nicht sauber." -ForegroundColor Yellow
    Write-Host "         Versuch wird trotzdem gestartet ..." -ForegroundColor Yellow
}

# --- 5. Image ROH auf die Platte schreiben (sektor-ausgerichtet) ---
Write-Host "Schreibe Image ... (nicht abbrechen!)"

# Hilfsfunktion: PhysicalDrive-Handle oeffnen mit Wiederholung.
# Bei USB-Sticks kann der Handle direkt nach Clear-Disk noch kurz
# scheitern, obwohl das Geraet schon wieder sichtbar ist.
function Open-RawDisk {
    param([int]$Nr, [int]$Versuche = 10, [int]$PauseSek = 2)
    $pfad = "\\.\PhysicalDrive$Nr"
    for ($v = 1; $v -le $Versuche; $v++) {
        try {
            $h = [System.IO.File]::Open($pfad, 'Open', 'Write', 'ReadWrite')
            return $h
        } catch [System.IO.IOException] {
            if ($v -ge $Versuche) { throw }
            Write-Host "  Geraet noch nicht bereit (Versuch $v/$Versuche), warte ${PauseSek}s ..." -ForegroundColor Yellow
            Start-Sleep -Seconds $PauseSek
        }
    }
}

$dev = $null; $img = $null
try {
    $img = [System.IO.File]::OpenRead($imgVoll)
    $dev = Open-RawDisk -Nr $diskNr
    $buf = New-Object byte[] (4 * 1024 * 1024)
    $gesamt = 0L
    while (($n = $img.Read($buf, 0, $buf.Length)) -gt 0) {
        # Auf 512-Byte-Sektoren auffuellen (letzter Block):
        $schreib = $n
        if ($schreib % 512 -ne 0) {
            $rest = 512 - ($schreib % 512)
            [Array]::Clear($buf, $schreib, $rest)
            $schreib += $rest
        }
        # Schreiben mit Retry: Falls der Handle waehrend des Schreibens
        # ungueltig wird (spaete Re-Enumeration), Handle neu oeffnen und
        # an der richtigen Stelle fortsetzen.
        $schreibOk = $false
        for ($sv = 1; $sv -le 3; $sv++) {
            try {
                $dev.Write($buf, 0, $schreib)
                $schreibOk = $true
                break
            } catch [System.IO.IOException] {
                if ($sv -ge 3) { throw }
                Write-Host ""
                Write-Host "  Schreibfehler (Versuch $sv/3): $($_.Exception.Message)" -ForegroundColor Yellow
                Write-Host "  Oeffne Geraet erneut und setze fort ..." -ForegroundColor Yellow
                # Alten Handle schliessen (best-effort):
                try { $dev.Close() } catch {}
                Start-Sleep -Seconds 3
                $dev = Open-RawDisk -Nr $diskNr
                # Seek auf die aktuelle Schreibposition (sektorausgerichtet):
                $seekPos = $gesamt
                if ($seekPos % 512 -ne 0) {
                    $seekPos = $gesamt - ($gesamt % 512)
                }
                $dev.Seek($seekPos, [System.IO.SeekOrigin]::Begin) | Out-Null
            }
        }
        if (-not $schreibOk) {
            throw "Schreiben auf den USB-Stick endgueltig fehlgeschlagen nach 3 Versuchen."
        }
        $gesamt += $n
        Write-Progress -Activity "Schreibe SpeedOS auf USB" -Status "$([math]::Round($gesamt/1MB,1)) MiB" `
            -PercentComplete ([math]::Min(100, [int]($gesamt * 100 / $imgLen)))
    }
    $dev.Flush()
} finally {
    if ($dev) { $dev.Close() }
    if ($img) { $img.Close() }
}
Write-Progress -Activity "Schreibe SpeedOS auf USB" -Completed

# --- 6. Platte neu einlesen (kein Offline noetig, s. o.) ---
Update-Disk -Number $ziel.Number

Write-Host ""
Write-Host "FERTIG. SpeedOS wurde auf Datentraeger $($ziel.Number) ($($ziel.FriendlyName)) geschrieben." -ForegroundColor Green
Write-Host "Stick sicher auswerfen, in den Laptop stecken, im BIOS/UEFI vom USB booten." -ForegroundColor Green
Write-Host "BIOS: Secure Boot AUS, UEFI-Boot (nicht Legacy/CSM). Details: docs/usb-boot.md"
