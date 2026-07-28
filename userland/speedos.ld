/* speedos.ld — Das Linker-Skript aller SpeedOS-Programme
 *
 * Es beantwortet die eine Frage, die ein statisch gelinktes Programm
 * beantworten muss: WO IM ADRESSRAUM LIEGE ICH?
 *
 * Bei SpeedOS ist die Antwort fest: bei 0x80_0000_0000 — das ist
 * `adressraum::USER_START` (512 GiB), der Anfang des einzigen P4-Slots, der
 * jedem Prozess privat gehört. Jedes Programm liegt an DERSELBEN Adresse,
 * und das ist kein Konflikt, sondern der ganze Witz von Adressraum-Trennung:
 * `hallo` und `netzhole` sehen beide 0x80_0000_0000, aber jeder sein eigenes.
 *
 * WARUM FESTE ADRESSEN (ET_EXEC) UND KEIN PIE: Ein positionsunabhängiges
 * Programm müsste beim Laden relokiert werden — der Kernel müsste
 * Relokations-Tabellen verstehen und Adressen einsetzen. Das ist ein
 * eigenes Teilprojekt ohne Gewinn, solange jeder Prozess seinen eigenen
 * Adressraum hat und sich niemand die Adresse teilen muss. `src/elf.rs`
 * lehnt ET_DYN deshalb ausdrücklich ab (mit einem eigenen Fehler, damit man
 * den Bau-Fehler sofort erkennt).
 *
 * DIE ALIGN(4096) SIND NICHT KOSMETIK: Sie zwingen den Linker, für jede
 * Rechte-Kombination ein EIGENES PT_LOAD-Segment auf EIGENEN Seiten
 * anzulegen. Nur dadurch kann der Loader .text als R-X und .data als RW-
 * mappen. Teilten sich zwei Segmente eine Seite, müssten sie sich die
 * Rechte teilen — und `elf::pruefen` lehnt genau das ab (W^X wäre sonst
 * aushebelbar).
 */

ENTRY(_start)

SECTIONS
{
    /* adressraum::USER_START — muss mit elf::IMAGE_START übereinstimmen. */
    . = 0x8000000000;

    /* Code: lesen + ausfuehren (R-X) */
    .text : ALIGN(4096)
    {
        *(.text._start)     /* der Einsprung zuerst — reine Lesbarkeit */
        *(.text .text.*)
    }

    /* Konstanten: nur lesen (R--) */
    . = ALIGN(4096);
    .rodata : ALIGN(4096)
    {
        *(.rodata .rodata.*)
    }

    /* Daten und BSS: lesen + schreiben, NICHT ausfuehrbar (RW-) */
    . = ALIGN(4096);
    .data : ALIGN(4096)
    {
        *(.data .data.*)
        *(.got .got.plt)
    }
    /* Auch die .bss bekommt eine EIGENE Seite. Das ist noetig, weil lld
     * fuer den NOBITS-Teil gern ein zweites PT_LOAD anlegt: Ohne diese
     * Ausrichtung faengt es mitten in der letzten .data-Seite an, und dann
     * beanspruchen zwei Segmente dieselbe Seite — was `elf::pruefen`
     * ablehnt. Es kostet nichts: .bss steht ohnehin nicht in der Datei. */
    . = ALIGN(4096);
    .bss : ALIGN(4096)
    {
        *(.bss .bss.*)
        *(COMMON)
    }

    /* Was wir nicht laden koennen und nicht brauchen. Insbesondere die
     * Abwickel-Tabellen (.eh_frame): panic = "abort", es wird nie
     * abgewickelt. */
    /DISCARD/ :
    {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
        *(.gcc_except_table*)
    }
}
