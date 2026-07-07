# SpeedOS — Projektregeln für Claude

## Projekt
- SpeedOS: ein eigenes Betriebssystem from scratch in Rust. Kein Linux, keine fremde Kernel-Basis.
- Sprache: Rust **nightly**, `no_std`, Ziel-Architektur: **x86_64**.
- Bootloader: das `bootloader`-Crate (**Version 0.9.x**), kompatibel mit dem Buch
  "Writing an OS in Rust" von Philipp Oppermann — wir folgen dessen bewährter Architektur.

## Build & Test-Umgebung
- Test-Umgebung: **QEMU** (`qemu-system-x86_64`), gestartet über `cargo run` mittels `bootimage`.
- Eigenes Target-JSON: `x86_64-speedos.json`.
- Tests: Integrationstests laufen in QEMU mit dem **isa-debug-exit Device** für
  automatisches Beenden mit Exit-Code (`cargo test`).

## Debug
- **ALLE** Debug-Ausgaben gehen über die serielle Schnittstelle (COM1 / Port 0x3F8)
  ins Terminal, zusätzlich zur VGA-Ausgabe. **Niemals nur VGA.**

## Git
- Nach **JEDEM** funktionierenden Schritt ein Commit mit klarer Message.
- **NIEMALS** committen, wenn `cargo build` fehlschlägt oder QEMU nicht bootet.

## Arbeitsweise
- Kleine Schritte. Nach jeder Änderung selbst bauen, in QEMU starten, serielle Ausgabe prüfen.
- Fehler selbst debuggen und fixen, bevor "fertig" gemeldet wird.
- Der Projektbesitzer ist Anfänger in OS-Entwicklung: nach jedem Schritt in 2–3 Sätzen
  auf Deutsch erklären, was gebaut wurde.

## Code-Stil
- Ausführliche **deutsche Kommentare**, da der Projektbesitzer OS-Entwicklung lernen will.
- Jede Datei beginnt mit einem Kommentarblock, der erklärt, was sie tut.

## Architektur-Prinzip
- Mikrokernel-inspiriert: Treiber und Systemdienste so isoliert wie möglich
  (eigene Module, klare Schnittstellen, so wenig geteilter Zustand wie möglich).
