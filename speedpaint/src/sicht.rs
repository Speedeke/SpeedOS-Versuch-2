// speedpaint::sicht — der Ausschnitt und das Scrollen
//
// ===========================================================================
// DIE SICHT IST DER GANZE ZUSTAND DES SCROLLENS
//
// Drei Zahlen: wo das Fenster auf der Leinwand liegt (`bereich`), wie
// hoch das gesetzte Dokument ist (`inhalt_hoehe`) und wie weit
// heruntergescrollt wurde (`versatz`). Mehr braucht es nicht — und weil
// es nicht mehr ist, ist jede Scroll-Frage eine Rechnung auf drei Zahlen
// und keine Zustandsmaschine mit Ecken.
//
// **Der Versatz ist der EINZIGE Weg, an dem sich Scrollen im Bild
// auswirkt.** Die Anzeigeliste bleibt Byte fuer Byte dieselbe (sie ist
// beim Malen eine `&`-Referenz), das Layout wird nicht angefasst. Das
// ist die Zusage von Aufgabe 2, und sie steht nicht in einem Kommentar,
// sondern in den Typen.
//
// ===========================================================================
// WARUM DAS SCROLLEN EINE **FOLGE** ZURUECKGIBT UND NICHT NUR EINE ZAHL
//
// Ein neuer Versatz allein sagt dem Aufrufer nicht, was er zu tun hat.
// Die teure Frage ist: Muss die ganze Flaeche neu gemalt werden, oder
// laesst sich der schon gemalte Teil VERSCHIEBEN und nur der neu
// sichtbare Streifen nachziehen? Das haengt davon ab, wie weit gesprungen
// wurde — und diese Entscheidung gehoert hierher, wo die Zahlen liegen,
// nicht in die Ereignisschleife des Browsers.
//
// EHRLICHE EINORDNUNG, die man beim Messen sofort merkt: Der Streifen
// spart das **Malen**, nicht die **Kopie**. Der Kernel hat eine eigene
// Kopie des Fensterpuffers; verschiebt der Prozess seine Pixel, weiss der
// Kernel nichts davon, und es muss trotzdem die ganze Flaeche uebertragen
// werden. Ein „Fenster scrollen"-Syscall gibt es nicht (und er waere ein
// Sonderfall im ABI fuer genau einen Anwendungsfall). Was das fuer das
// Umstiegskriterium bedeutet, steht in docs/browser-rendern.md.

use speedui::Rechteck;

/// Wie viele Textzeilen eine Rastung des Mausrads bewegt.
///
/// DIESELBE ZAHL WIE IM TERMINAL-RUECKBLICK (`fenster/terminal.rs`,
/// Juli 2026). Nicht weil drei besonders richtig waere, sondern weil
/// zwei verschiedene Scroll-Geschwindigkeiten im selben System ein
/// Bedienfehler sind.
pub const ZEILEN_JE_RASTUNG: i32 = 3;

/// Ersatz-Zeilenhoehe, wenn niemand eine gesetzt hat.
pub const STANDARD_ZEILENHOEHE: i32 = 20;

/// Ein Scroll-Wunsch. Was die Eingabe bedeutet, wird EINMAL hier
/// uebersetzt — die Ereignisschleife des Browsers ordnet nur Tasten zu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scrollschritt {
    ZeileHoch,
    ZeileRunter,
    SeiteHoch,
    SeiteRunter,
    /// Pos1 — an den Anfang.
    Anfang,
    /// Ende — ans Dokumentende.
    Ende,
    /// Mausrad, in Rastungen.
    ///
    /// **POSITIV = NACH OBEN**, also zum Dokumentanfang — dieselbe
    /// Richtung wie in `maus.rs` („Scrollrad: positiv = nach oben,
    /// negativ = nach unten") und wie im Terminal-Rueckblick.
    ///
    /// Die erste Fassung hatte es andersherum (positiv = weiterlesen),
    /// was fuer sich genommen genauso vertretbar war — und deshalb ist
    /// der Fehler lehrreich: Zwei Vorzeichen-Konventionen fuer dasselbe
    /// Geraet im selben System sind ein Bedienfehler, egal welche
    /// einzeln die schoenere ist. Aufgefallen ist es erst am
    /// Bildschirm, weil beide Testfaelle zufaellig an einer Klemmung
    /// standen: Am Anfang nach oben und am Ende nach unten zu scrollen
    /// tut in JEDER Konvention nichts. Seitdem prueft
    /// `test_rad_richtung` aus der Mitte heraus.
    Rad(i32),
    /// Ein absoluter Versatz — der Scrollbalken beim Ziehen.
    Nach(i32),
}

/// Was ein Scroll-Schritt bewirkt hat, und was daraufhin zu tun ist.
///
/// Der Aufrufer braucht alle vier Angaben: die Verschiebung, um seine
/// Pixel zu bewegen, den Streifen, um ihn neu zu malen, und `alles`, um
/// zu wissen, dass sich das Verschieben nicht lohnt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Folge {
    pub vorher: i32,
    pub nachher: i32,
    /// `nachher - vorher`. Positiv heisst: der Inhalt wandert nach OBEN,
    /// unten wird etwas Neues sichtbar.
    pub verschiebung: i32,
    /// Was neu gemalt werden muss (Leinwand-Koordinaten). `None` = es hat
    /// sich nichts geaendert.
    pub streifen: Option<Rechteck>,
    /// `true`, wenn der Sprung so gross war, dass die ganze Flaeche neu
    /// gemalt wird — dann ist Verschieben sinnlos.
    pub alles: bool,
}

impl Folge {
    /// Hat sich ueberhaupt etwas geaendert?
    pub fn geaendert(&self) -> bool {
        self.verschiebung != 0
    }
    /// Lohnt sich das Verschieben der schon gemalten Pixel?
    pub fn verschieben_lohnt(&self) -> bool {
        !self.alles && self.verschiebung != 0
    }
}

/// Der sichtbare Ausschnitt eines gesetzten Dokuments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sicht {
    /// Wo der Inhalt auf der Leinwand liegt (Fensterinhalt-Koordinaten).
    pub bereich: Rechteck,
    /// Hoehe des gesetzten Dokuments in Pixeln.
    pub inhalt_hoehe: i32,
    /// Wie weit heruntergescrollt (>= 0, <= max_versatz).
    versatz: i32,
    /// Fuer Zeilen-Schritte. Kommt aus der Metrik des Wirts.
    pub zeilen_hoehe: i32,
}

impl Sicht {
    pub fn neu(bereich: Rechteck, inhalt_hoehe: i32) -> Sicht {
        Sicht {
            bereich,
            inhalt_hoehe: inhalt_hoehe.max(0),
            versatz: 0,
            zeilen_hoehe: STANDARD_ZEILENHOEHE,
        }
    }

    pub fn mit_zeilenhoehe(mut self, hoehe: i32) -> Sicht {
        self.zeilen_hoehe = hoehe.max(1);
        self
    }

    pub fn versatz(&self) -> i32 {
        self.versatz
    }

    /// Der groesste sinnvolle Versatz: so weit, dass die letzte
    /// Dokumentzeile unten steht — nie weiter.
    ///
    /// Ist das Dokument kuerzer als das Fenster, ist er 0. Ohne das
    /// `max(0)` koennte man ein kurzes Dokument aus dem Bild
    /// herausscrollen, und der Benutzer saehe eine leere Flaeche ohne
    /// Erklaerung.
    pub fn max_versatz(&self) -> i32 {
        (self.inhalt_hoehe - self.bereich.hoehe).max(0)
    }

    /// Ist ueberhaupt etwas zu scrollen?
    pub fn scrollbar(&self) -> bool {
        self.max_versatz() > 0
    }

    /// Der sichtbare Bereich in DOKUMENT-Koordinaten.
    pub fn dokument_fenster(&self) -> Rechteck {
        Rechteck::neu(0, self.versatz, self.bereich.breite, self.bereich.hoehe)
    }

    /// Rechnet eine Dokument-Y-Koordinate in eine Leinwand-Koordinate um.
    #[inline]
    pub fn nach_leinwand_y(&self, dokument_y: i32) -> i32 {
        dokument_y - self.versatz + self.bereich.y
    }

    /// Rechnet eine Leinwand-Y-Koordinate zurueck ins Dokument — fuer
    /// Klick-Ziele.
    #[inline]
    pub fn nach_dokument_y(&self, leinwand_y: i32) -> i32 {
        leinwand_y - self.bereich.y + self.versatz
    }

    /// KLEMMT einen Wunsch-Versatz in den erlaubten Bereich.
    ///
    /// Die einzige Stelle, an der geklemmt wird. Jeder Weg zu einem neuen
    /// Versatz laeuft hier durch — deshalb kann kein Scroll-Weg (Rad,
    /// Taste, Balken, Groessenaenderung) an Anfang oder Ende
    /// vorbeischiessen.
    pub fn klemmen(&self, wunsch: i32) -> i32 {
        wunsch.clamp(0, self.max_versatz())
    }

    /// Fuehrt einen Scroll-Schritt aus und sagt, was zu tun ist.
    pub fn scrollen(&mut self, schritt: Scrollschritt) -> Folge {
        let ziel = self.ziel_von(schritt);
        self.versatz_setzen(ziel)
    }

    /// Welchen Versatz will dieser Schritt? (Noch ungeklemmt — das
    /// besorgt `versatz_setzen`.)
    fn ziel_von(&self, schritt: Scrollschritt) -> i32 {
        let zeile = self.zeilen_hoehe.max(1);
        // Eine Seite laesst BEWUSST eine Zeile stehen: Wer blaettert,
        // will den Anschluss sehen. Bei sehr kleinen Fenstern darf daraus
        // aber kein Stillstand werden, deshalb mindestens eine Zeile.
        let seite = (self.bereich.hoehe - zeile).max(zeile);
        match schritt {
            Scrollschritt::ZeileHoch => self.versatz - zeile,
            Scrollschritt::ZeileRunter => self.versatz + zeile,
            Scrollschritt::SeiteHoch => self.versatz - seite,
            Scrollschritt::SeiteRunter => self.versatz + seite,
            Scrollschritt::Anfang => 0,
            Scrollschritt::Ende => self.max_versatz(),
            // MINUS, weil positiv „nach oben" heisst (siehe oben).
            //
            // `saturating_*`, weil ein boeses oder verrutschtes Rad-Delta
            // sonst ueberlaufen koennte. Ueberlauf waere hier ein Sprung
            // ans andere Ende des Dokuments.
            Scrollschritt::Rad(rastungen) => self
                .versatz
                .saturating_sub(rastungen.saturating_mul(ZEILEN_JE_RASTUNG.saturating_mul(zeile))),
            Scrollschritt::Nach(wunsch) => wunsch,
        }
    }

    /// Setzt den Versatz (geklemmt) und rechnet die Folge aus.
    pub fn versatz_setzen(&mut self, wunsch: i32) -> Folge {
        let vorher = self.versatz;
        let nachher = self.klemmen(wunsch);
        self.versatz = nachher;
        self.folge(vorher, nachher)
    }

    /// Die Folge einer Versatz-Aenderung: verschieben und/oder malen?
    fn folge(&self, vorher: i32, nachher: i32) -> Folge {
        let verschiebung = nachher - vorher;
        if verschiebung == 0 {
            return Folge {
                vorher,
                nachher,
                verschiebung: 0,
                streifen: None,
                alles: false,
            };
        }
        let hoehe = self.bereich.hoehe;
        let weite = verschiebung.abs();
        if weite >= hoehe {
            // Der Sprung ist mindestens so gross wie das Fenster — vom
            // alten Bild bleibt nichts stehen. Verschieben waere reine
            // Arbeit ohne Gewinn.
            return Folge {
                vorher,
                nachher,
                verschiebung,
                streifen: Some(self.bereich),
                alles: true,
            };
        }
        // Nur der neu sichtbare Rand muss gemalt werden: unten, wenn nach
        // unten gescrollt wurde, sonst oben.
        let streifen = if verschiebung > 0 {
            Rechteck::neu(
                self.bereich.x,
                self.bereich.y + hoehe - weite,
                self.bereich.breite,
                weite,
            )
        } else {
            Rechteck::neu(self.bereich.x, self.bereich.y, self.bereich.breite, weite)
        };
        Folge {
            vorher,
            nachher,
            verschiebung,
            streifen: Some(streifen),
            alles: false,
        }
    }

    /// Nach einer Groessenaenderung oder einem neuen Layout: Masse
    /// uebernehmen und den Versatz neu klemmen.
    ///
    /// WARUM DAS NOETIG IST: Wird ein Fenster hoeher gezogen oder das
    /// Dokument beim Neu-Layout kuerzer (breiteres Fenster = weniger
    /// Umbrueche = weniger Zeilen), kann der bisherige Versatz hinter dem
    /// Dokumentende liegen. Ohne dieses Nachklemmen zeigte der Browser
    /// eine leere Flaeche, und Scrollen nach unten haette keine Wirkung
    /// mehr — ein Zustand, aus dem der Benutzer nicht mehr herausfindet.
    pub fn anpassen(&mut self, bereich: Rechteck, inhalt_hoehe: i32) {
        self.bereich = bereich;
        self.inhalt_hoehe = inhalt_hoehe.max(0);
        self.versatz = self.klemmen(self.versatz);
    }

    // -----------------------------------------------------------------
    // DER SCROLLBALKEN
    // -----------------------------------------------------------------

    /// Spur und Greifer des Scrollbalkens — oder `None`, wenn es nichts
    /// zu scrollen gibt.
    ///
    /// KEIN BALKEN BEI KURZEN SEITEN, und das ist eine Aussage und keine
    /// Ersparnis: Ein Balken, der die ganze Spur fuellt, behauptet, man
    /// koenne scrollen.
    pub fn balken(&self, breite: i32) -> Option<Balken> {
        if !self.scrollbar() || breite <= 0 {
            return None;
        }
        let spur = Rechteck::neu(
            self.bereich.x + self.bereich.breite - breite,
            self.bereich.y,
            breite,
            self.bereich.hoehe,
        );
        // Der Greifer ist so lang, wie das Fenster am Dokument Anteil
        // hat — die uebliche und einzige Anzeige, die dem Benutzer
        // verraet, wie lang die Seite ist.
        let anteil = (self.bereich.hoehe as i64 * spur.hoehe as i64)
            / (self.inhalt_hoehe.max(1) as i64);
        // MINDESTLAENGE: Bei einem sehr langen Dokument faellt der
        // Greifer sonst auf null Pixel und ist unsichtbar UND nicht
        // greifbar. 24 px ist etwa eine Fingerbreite auf dem Schirm.
        let greifer_hoehe = (anteil as i32).clamp(MIN_GREIFER.min(spur.hoehe), spur.hoehe);
        let rest = spur.hoehe - greifer_hoehe;
        let greifer_y = if self.max_versatz() == 0 {
            0
        } else {
            ((self.versatz as i64 * rest as i64) / self.max_versatz() as i64) as i32
        };
        Some(Balken {
            spur,
            greifer: Rechteck::neu(spur.x, spur.y + greifer_y, breite, greifer_hoehe),
        })
    }

    /// Der Versatz, der zu einem Klick auf die Spur gehoert.
    ///
    /// Der Klickpunkt wird als MITTE des Greifers gedeutet — sonst
    /// springt der Inhalt beim Anfassen des Balkens.
    pub fn versatz_aus_balken(&self, leinwand_y: i32, balken: &Balken) -> i32 {
        let rest = balken.spur.hoehe - balken.greifer.hoehe;
        if rest <= 0 {
            return self.versatz;
        }
        let oben = leinwand_y - balken.spur.y - balken.greifer.hoehe / 2;
        self.klemmen(((oben.max(0) as i64 * self.max_versatz() as i64) / rest as i64) as i32)
    }
}

/// Mindestlaenge des Greifers in Pixeln.
pub const MIN_GREIFER: i32 = 24;

/// Die Geometrie des Scrollbalkens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Balken {
    pub spur: Rechteck,
    pub greifer: Rechteck,
}
