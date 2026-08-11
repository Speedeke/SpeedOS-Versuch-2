// browser::seiten — die Seiten, die der Browser selbst schreibt
//
// ===========================================================================
// SIE SIND ECHTES HTML, UND DAS IST DER PUNKT
//
// Fehlermeldungen und die Info-Seite koennten auch direkt gezeichnet
// werden. Sie sind stattdessen HTML, das durch DIESELBE Kette laeuft wie
// jede fremde Seite (speedhtml -> speedcss -> speedlayout -> speedpaint).
//
// Drei Gruende:
//   1. Sie sind der SELBSTTEST der Kette bei jedem Fehlschlag. Wenn eine
//      Fehlerseite falsch aussieht, ist der Renderer kaputt — und man
//      sieht es sofort, statt es zu suchen.
//   2. Sie scrollen, brechen um und skalieren mit der Fensterbreite, ohne
//      dass dafuer eine Zeile geschrieben werden muesste.
//   3. Es gibt keinen zweiten Zeichenweg, der gepflegt werden will.
//
// ===========================================================================
// DER TON
//
// Eine Fehlerseite sagt, WAS passiert ist, WORAN es lag und WAS man tun
// kann — in dieser Reihenfolge, in ganzen Saetzen. Was sie NICHT tut: dem
// Benutzer anbieten, die Pruefung zu umgehen. Bei Zertifikatsfehlern ist
// das eine harte Regel aus Serie 7 (docs/grenzen.md, „Was ausdruecklich
// KEINE Luecke ist"): Es gibt keinen `--unsicher`-Schalter und keinen
// „trotzdem fortfahren"-Knopf. Ein Knopf wird benutzt, sobald es ihn
// gibt.

use alloc::string::String;

/// Das gemeinsame Stylesheet der eingebauten Seiten.
///
/// Bewusst klein und ausschliesslich aus dem, was `speedcss` wirklich
/// kann (docs/browser-v1.md §2.3) — eine eingebaute Seite, die
/// Eigenschaften benutzt, die wir nicht unterstuetzen, waere eine
/// merkwuerdige Art, sich selbst zu widerlegen.
const STIL: &str = "\
body { background: #ffffff; color: #202020; margin: 24px; }
h1 { font-size: 28px; color: #303030; }
h2 { font-size: 20px; color: #404040; }
.kasten { background: #f4f4f4; border: 1px solid #d0d0d0; padding: 12px; margin: 12px 0; }
.warnung { background: #fff0f0; border: 1px solid #d08080; padding: 12px; margin: 12px 0; }
.gut { color: #206020; }
.fehlt { color: #903030; }
code { background: #eeeeee; }
";

/// Baut ein vollstaendiges Dokument mit Titel und Rumpf.
fn seite(titel: &str, rumpf: &str) -> String {
    let mut s = String::from("<html><head><title>");
    s.push_str(titel);
    s.push_str("</title><style>");
    s.push_str(STIL);
    s.push_str("</style></head><body>");
    s.push_str(rumpf);
    s.push_str("</body></html>");
    s
}

/// HTML-Sonderzeichen entschaerfen.
///
/// NOETIG, WEIL FREMDER TEXT HINEINGERAET: Eine Fehlerseite zeigt die
/// URL, die der Benutzer getippt hat, und eine Verlaufsseite zeigt
/// Seitentitel von fremden Servern. Stuende dort ein `<`, wuerde unser
/// eigener Parser es als Tag lesen — die Seite saehe kaputt aus, und im
/// schlimmsten Fall verschluckte ein `<script>` aus einem Seitentitel
/// den Rest unserer eigenen Seite.
pub fn maskieren(text: &str) -> String {
    let mut aus = String::with_capacity(text.len());
    for zeichen in text.chars() {
        match zeichen {
            '<' => aus.push_str("&lt;"),
            '>' => aus.push_str("&gt;"),
            '&' => aus.push_str("&amp;"),
            '"' => aus.push_str("&quot;"),
            anderes => aus.push(anderes),
        }
    }
    aus
}

// ===========================================================================
// DIE INFO-SEITE — der Ehrlichkeits-Teil
// ===========================================================================

/// `speedos:info` — was dieser Browser kann und was nicht.
///
/// ===================================================================
/// WARUM DIESE SEITE EXISTIERT
///
/// Ein Browser, der 90 % der Seiten falsch darstellt, ist entweder
/// kaputt oder ehrlich. Der Unterschied ist, ob er sagt, WELCHE 10 % er
/// kann. Diese Seite ist die Antwort — und sie steht ABSICHTLICH im
/// Browser selbst und nicht nur in docs/grenzen.md: Wer sie braucht,
/// sitzt gerade vor einer Seite, die komisch aussieht.
pub fn info() -> String {
    let rumpf = "\
<h1>SpeedOS-Browser</h1>
<p>Ein Browser, der in einem <b>unprivilegierten Prozess</b> laeuft und auf
nichts aufbaut ausser SpeedOS selbst: eigener Kernel, eigener TCP/IP-Stack,
eigener TLS-Weg, eigener HTML-Parser, eigenes Layout, eigener Renderer.</p>

<h2>Was funktioniert</h2>
<div class='kasten'>
<p class='gut'>HTML5-Parser mit Fehlererholung - jede Bytefolge ergibt einen Baum.</p>
<p class='gut'>CSS: Kaskade, Vererbung, Spezifitaet, ein Standard-Stylesheet.</p>
<p class='gut'>Layout: Bloecke, Zeilenumbruch, Tabellen, Listen, Bilder.</p>
<p class='gut'>HTTP und HTTPS (TLS 1.2/1.3), Weiterleitungen, Zertifikatspruefung.</p>
<p class='gut'>Tabs, Verlauf mit Zurueck/Vor, Lesezeichen, Scrollen.</p>
<p class='gut'>PNG und JPEG, in Ring 3 dekodiert.</p>
</div>

<h2>Was NICHT funktioniert</h2>
<div class='warnung'>
<p class='fehlt'><b>Kein JavaScript.</b> Gar keins. Eine Seite, die ihren Inhalt
erst per Skript aufbaut, bleibt hier leer - der Browser sagt es dann
ausdruecklich, statt eine weisse Flaeche zu zeigen.</p>
<p class='fehlt'>Kein Flexbox, kein Grid, keine Positionierung, keine Animationen.</p>
<p class='fehlt'>Formulare werden angezeigt, aber nicht abgeschickt.</p>
<p class='fehlt'>Keine Cookies, keine Anmeldung, kein Video, kein Audio.</p>
<p class='fehlt'>Schrift: ein 5x7-Raster, nur Grossbuchstaben, keine Umlaute.
Die Breiten stimmen trotzdem - Zeilen brechen an der richtigen Stelle um.</p>
<p class='fehlt'>Keine Sperrlisten-Pruefung (OCSP/CRL) bei Zertifikaten.</p>
</div>

<h2>Sicherheit</h2>
<div class='kasten'>
<p>Zertifikate werden gegen ein mitgeliefertes Wurzel-Buendel geprueft, und
zwar <b>immer</b>. Es gibt <b>keinen</b> Weg, eine fehlgeschlagene Pruefung zu
uebergehen - keinen Schalter, keinen Knopf, keine Ausnahmeliste. Wenn ein
Zertifikat nicht stimmt, wird die Seite nicht geladen.</p>
<p>Geht die Uhr unplausibel falsch, wird die Gueltigkeitspruefung nicht etwa
uebersprungen, sondern die Verbindung abgelehnt.</p>
</div>

<h2>Bedienung</h2>
<div class='kasten'>
<p><code>Strg+T</code> neuer Tab   <code>Strg+W</code> Tab schliessen
  <code>Strg+H</code> Verlauf   <code>Strg+D</code> Lesezeichen setzen
  <code>Strg+L</code> Adressleiste   <code>F5</code> neu laden</p>
<p>Scrollen: Mausrad, Pfeile, Bild auf/ab, Pos1/Ende, Leertaste.</p>
</div>
";
    seite("SpeedOS-Browser", rumpf)
}

// ===========================================================================
// FEHLERSEITEN
// ===========================================================================

/// Eine gewoehnliche Fehlerseite: Ueberschrift, Erklaerung, Adresse.
pub fn fehler(ueberschrift: &str, erklaerung: &str, adresse: &str) -> String {
    let mut rumpf = String::from("<h1>");
    rumpf.push_str(&maskieren(ueberschrift));
    rumpf.push_str("</h1><div class='kasten'><p>");
    rumpf.push_str(&maskieren(erklaerung));
    rumpf.push_str("</p><p><b>Adresse:</b> <code>");
    rumpf.push_str(&maskieren(adresse));
    rumpf.push_str("</code></p></div>");
    seite("Fehler", &rumpf)
}

/// Die Fehlerseite fuer SICHERHEITSFEHLER — bewusst anders.
///
/// ===================================================================
/// LAUT UND UNUMGEHBAR
///
/// Ein Zertifikatsfehler ist kein Netzproblem. Er heisst: Die
/// Gegenstelle ist moeglicherweise nicht die, die sie zu sein behauptet.
/// Deshalb sieht diese Seite anders aus als „Server nicht erreichbar",
/// und deshalb steht auf ihr KEIN Knopf, der weiterfuehrt.
///
/// Das ist die Dauerregel aus Serie 7, Teil 4: kein `--unsicher`, kein
/// `--zertifikat-egal`, kein „trotzdem fortfahren". Der Satz „Ein
/// Schalter wird benutzt, sobald es ihn gibt" gilt fuer Knoepfe genauso.
pub fn sicherheitsfehler(kurz: &str, erklaerung: &str, adresse: &str) -> String {
    let mut rumpf = String::from("<h1>Verbindung nicht sicher</h1><div class='warnung'><p><b>");
    rumpf.push_str(&maskieren(erklaerung));
    rumpf.push_str("</b></p><p><b>Adresse:</b> <code>");
    rumpf.push_str(&maskieren(adresse));
    rumpf.push_str("</code><br>Befund: <code>");
    rumpf.push_str(&maskieren(kurz));
    rumpf.push_str(
        "</code></p></div>\
<div class='kasten'>\
<p>SpeedOS hat die Seite <b>nicht geladen</b>. Die Identitaet der Gegenstelle \
liess sich nicht bestaetigen - jemand koennte sich dazwischen befinden.</p>\
<p>Es gibt hier <b>keinen Weg, das zu uebergehen</b>. Das ist Absicht und kein \
fehlendes Feature: Ein Knopf, der eine fehlgeschlagene Pruefung uebergeht, wird \
benutzt - und dann war die Pruefung nichts wert.</p>\
<p>Was hilft: die Adresse pruefen, spaeter erneut versuchen, oder bei einem \
abgelaufenen Zertifikat die Systemuhr kontrollieren.</p>\
</div>",
    );
    seite("Verbindung nicht sicher", &rumpf)
}

/// Der Hinweis fuer eine Seite, die ohne JavaScript leer bleibt.
///
/// ===================================================================
/// BESSER ALS EINE WEISSE FLAECHE
///
/// Der haeufigste Fall bei einem Browser ohne JavaScript ist nicht ein
/// Absturz, sondern NICHTS: Die Seite laedt, der Parser ist zufrieden,
/// das Layout rechnet — und es steht kein einziges Wort darin, weil der
/// ganze Inhalt per Skript nachgeladen wird.
///
/// Wer dann eine leere Seite zeigt, laesst den Benutzer glauben, der
/// Browser sei kaputt. Wer stattdessen sagt „diese Seite braucht
/// JavaScript, und das habe ich nicht", hat dieselbe Faehigkeit und eine
/// ehrliche Oberflaeche.
pub fn braucht_javascript(adresse: &str, skripte: usize) -> String {
    let mut rumpf = String::from("<h1>Diese Seite braucht JavaScript</h1><div class='kasten'>");
    rumpf.push_str(
        "<p>Die Seite wurde geladen und verstanden - sie enthaelt aber \
         <b>keinen sichtbaren Text</b>. Ihr Inhalt wird offenbar erst per \
         JavaScript aufgebaut, und <b>SpeedOS hat kein JavaScript</b>.</p>",
    );
    rumpf.push_str("<p>Gefunden: <code>");
    rumpf.push_str(&zahl(skripte));
    rumpf.push_str("</code> Skript-Bloecke, aber kein Text im Rumpf.</p>");
    rumpf.push_str("<p><b>Adresse:</b> <code>");
    rumpf.push_str(&maskieren(adresse));
    rumpf.push_str("</code></p></div>");
    rumpf.push_str(
        "<div class='kasten'><p>Was hilft: eine einfachere Fassung der Seite suchen \
         (viele Angebote haben eine), oder eine andere Quelle. Was dieser Browser \
         kann und was nicht, steht auf <code>speedos:info</code>.</p></div>",
    );
    seite("Braucht JavaScript", &rumpf)
}

// ===========================================================================
// VERLAUF UND LESEZEICHEN
// ===========================================================================

/// Die Verlaufsseite eines Tabs (Strg+H).
pub fn verlauf(eintraege: &[(String, String)], stelle: usize) -> String {
    let mut rumpf = String::from("<h1>Verlauf</h1>");
    if eintraege.is_empty() {
        rumpf.push_str("<div class='kasten'><p>Noch nichts besucht.</p></div>");
        return seite("Verlauf", &rumpf);
    }
    rumpf.push_str("<div class='kasten'><ul>");
    // NEUESTE ZUERST — so sucht man im Verlauf.
    for (i, (adresse, titel)) in eintraege.iter().enumerate().rev() {
        rumpf.push_str("<li><a href=\"");
        rumpf.push_str(&maskieren(adresse));
        rumpf.push_str("\">");
        rumpf.push_str(&maskieren(if titel.is_empty() { adresse } else { titel }));
        rumpf.push_str("</a>");
        if i == stelle {
            rumpf.push_str(" <b>(hier)</b>");
        }
        rumpf.push_str("<br><code>");
        rumpf.push_str(&maskieren(adresse));
        rumpf.push_str("</code></li>");
    }
    rumpf.push_str("</ul></div>");
    seite("Verlauf", &rumpf)
}

/// Die Lesezeichen-Seite.
pub fn lesezeichen(eintraege: &[(String, String)], pfad: &str) -> String {
    let mut rumpf = String::from("<h1>Lesezeichen</h1>");
    if eintraege.is_empty() {
        rumpf.push_str(
            "<div class='kasten'><p>Noch keine Lesezeichen. \
             Mit <code>Strg+D</code> die aktuelle Seite merken.</p></div>",
        );
    } else {
        rumpf.push_str("<div class='kasten'><ul>");
        for (adresse, titel) in eintraege {
            rumpf.push_str("<li><a href=\"");
            rumpf.push_str(&maskieren(adresse));
            rumpf.push_str("\">");
            rumpf.push_str(&maskieren(if titel.is_empty() { adresse } else { titel }));
            rumpf.push_str("</a><br><code>");
            rumpf.push_str(&maskieren(adresse));
            rumpf.push_str("</code></li>");
        }
        rumpf.push_str("</ul></div>");
    }
    rumpf.push_str("<div class='kasten'><p>Gespeichert in <code>");
    rumpf.push_str(&maskieren(pfad));
    rumpf.push_str("</code> - die Datei ueberlebt den Neustart.</p></div>");
    seite("Lesezeichen", &rumpf)
}

/// Eine Zahl als Text (kein `format!` — das zieht viel Code nach).
pub fn zahl(wert: usize) -> String {
    if wert == 0 {
        return String::from("0");
    }
    let mut ziffern = [0u8; 20];
    let mut i = 20;
    let mut rest = wert;
    while rest > 0 {
        i -= 1;
        ziffern[i] = b'0' + (rest % 10) as u8;
        rest /= 10;
    }
    let mut aus = String::new();
    for &z in &ziffern[i..] {
        aus.push(z as char);
    }
    aus
}

/// Die Liste der Textbefehle einer Seite auf „ist da ueberhaupt etwas?"
/// pruefen.
///
/// Ein Leerzeichen ist kein Inhalt, und ein einzelnes Zeichen auch nicht
/// — manche Seiten haben ein ` ` im leeren Rumpf stehen.
pub fn hat_sichtbaren_text(texte: &[&str]) -> bool {
    let mut zeichen = 0usize;
    for text in texte {
        zeichen += text.chars().filter(|z| !z.is_whitespace()).count();
        if zeichen > 8 {
            return true;
        }
    }
    false
}
