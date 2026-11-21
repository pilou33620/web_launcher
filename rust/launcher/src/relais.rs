//! Relais : ouvre un web tool à travers le launcher, à l'adresse /o/<id>/.
//!
//! Pourquoi : depuis Internet (Tailscale Funnel), une seule adresse https est joignable. Les outils,
//! eux, écoutent sur leurs propres ports (8150, 8080...), inaccessibles de l'extérieur. Le launcher
//! fait donc passer leurs pages par lui, après sa connexion, sur une adresse à part
//! (127.0.0.1:<port_relais>, derrière Caddy et le port 8443 de Funnel) : un outil relayé n'a pas
//! l'origine du launcher et ne peut pas appeler son API.
//!
//! - /o/<id>/...  : relayé vers l'outil, sur 127.0.0.1:<port>, préfixe retiré.
//! - Un outil écrit souvent des liens absolus ("/api/x", "/css/a.css") : ils sont réécrits dans le
//!   HTML, et ceux que le HTML ne contient pas (fetch, CSS) sont retrouvés grâce au Referer (la page
//!   d'où part la requête est sous /o/<id>/).
//! - WebSocket : retrouvé par le cookie `relais_outil` (la poignée de main n'envoie pas de Referer),
//!   puis tunnel brut dans les deux sens.
//! - Le cookie de session du launcher n'est jamais transmis à l'outil, ni l'en-tête Authorization.
//! - Un outil ne peut ni poser les cookies du launcher, ni installer un service worker.
//! - Envoi (POST, PUT...) et WebSocket : l'Origin doit être l'adresse du launcher.
//!
//! Ce serveur-ci est le sien (HTTP seulement, 127.0.0.1) : le serveur commun ne sait pas passer
//! une connexion en tunnel.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use regex::bytes::Regex as RegexOctets;
use regex::Regex;

use crate::serveur::{App, Vue, COOKIE};

pub const PREFIXE: &str = "/o/";
pub const COOKIE_OUTIL: &str = "relais_outil";
const HTML_MAX: u64 = 5 * 1024 * 1024;
const ATTENTE_TUNNEL: u64 = 600;
const ENTETES_MAX: usize = 64 * 1024;

/// En-têtes qui ne se transmettent pas tels quels (propres à la connexion, ou réécrits ici).
const SAUF_REQUETE: [&str; 21] = [
    "host", "connection", "keep-alive", "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade",
    "content-length", "accept-encoding", "authorization", "cookie", "origin", "referer", "expect", "tailscale-user-login",
    "tailscale-user-name", "tailscale-user-profile-pic", "tailscale-funnel-request", "tailscale-headers-info",
];
const SAUF_REPONSE: [&str; 14] = [
    "connection", "keep-alive", "proxy-authenticate", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade",
    "content-length", "content-encoding", "referrer-policy", "service-worker-allowed", "clear-site-data", "strict-transport-security",
];
const COOKIES_LAUNCHER: [&str; 2] = [COOKIE, COOKIE_OUTIL];
const SANS_EFFET: [&str; 3] = ["GET", "HEAD", "OPTIONS"];

pub fn prefixe(id: &str) -> String {
    format!("{}{}/", PREFIXE, id)
}

fn id_permis(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// urlparse : (netloc, chemin, requête, fragment).
fn decouper_url(url: &str) -> (&str, &str, &str, &str) {
    let (reste, fragment) = url.split_once('#').unwrap_or((url, ""));
    let (reste, requete) = reste.split_once('?').unwrap_or((reste, ""));
    let apres_schema = match reste.find("://") {
        Some(i) if i > 0 && reste[..i].bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b)) => Some(&reste[i + 1..]),
        _ => None,
    };
    match apres_schema.or_else(|| reste.starts_with("//").then_some(reste)) {
        Some(r) => {
            let r = &r[2..];
            let fin = r.find('/').unwrap_or(r.len());
            (&r[..fin], &r[fin..], requete, fragment)
        }
        None => ("", reste, requete, fragment),
    }
}

/// L'outil désigné par un chemin /o/<id>[/...] : (id, la suite à partir de « / »).
fn dans_prefixe(chemin: &str) -> Option<(&str, Option<&str>)> {
    let reste = chemin.strip_prefix(PREFIXE)?;
    let (id, suite) = match reste.find('/') {
        Some(i) => (&reste[..i], Some(&reste[i..])),
        None => (reste, None),
    };
    id_permis(id).then_some((id, suite))
}

/// L'outil d'où part la requête, d'après le chemin du Referer (/o/<id>/...), ou None. Le nom
/// d'hôte n'est pas comparé : derrière Funnel et Caddy, celui que voit le launcher varie, et ce
/// serveur ne sert que des outils (la session, cookie Strict, reste exigée).
fn id_du_referer(vue: &Vue) -> Option<String> {
    dans_prefixe(decouper_url(vue.entete("Referer").unwrap_or("")).1).map(|(id, _)| id.to_string())
}

/// (id, chemin pour l'outil) si cette requête va à un outil, sinon None.
pub fn cible(chemin: &str, vue: &Vue) -> Option<(String, String)> {
    if !chemin.starts_with('/') {
        return None;
    }
    if let Some((id, _)) = dans_prefixe(decouper_url(chemin).1) {
        let reste = &chemin[PREFIXE.len() + id.len()..]; // "" (sans barre finale), "/..." ou "?..."
        return Some((id.to_string(), if reste.starts_with('?') { format!("/{}", reste) } else { reste.to_string() }));
    }
    if chemin.starts_with(PREFIXE) {
        return None;
    }
    // sans Referer (WebSocket, fetch « no-referrer ») : le dernier outil ouvert dans ce navigateur
    let id = id_du_referer(vue).or_else(|| vue.cookie(COOKIE_OUTIL).filter(|v| id_permis(v)))?;
    Some((id, chemin.to_string()))
}

fn chemin_outil(chemin: &str) -> String {
    if !chemin.starts_with('/') || chemin.starts_with("//") {
        format!("/{}", chemin.trim_start_matches('/'))
    } else {
        chemin.to_string()
    }
}

fn entetes_vers_outil(vue: &Vue, hote: &str, tunnel: bool) -> Vec<(String, String)> {
    let mut sortie: Vec<(String, String)> = vue
        .entetes
        .iter()
        .filter(|(k, _)| {
            let bas = k.to_lowercase();
            !SAUF_REQUETE.contains(&bas.as_str()) || (tunnel && (bas == "connection" || bas == "upgrade"))
        })
        .cloned()
        .collect();
    sortie.push(("Host".into(), hote.into()));
    if let Some(origine) = vue.entete("Origin") {
        sortie.push(("Origin".into(), if origine != "null" { format!("http://{}", hote) } else { origine.into() }));
    }
    if let Some(r) = vue.entete("Referer") {
        let (_, p, q, _) = decouper_url(r);
        let chemin = match dans_prefixe(p) {
            Some((_, suite)) => suite.unwrap_or("/").to_string(),
            None if p.is_empty() => "/".into(),
            None => p.to_string(),
        };
        sortie.push(("Referer".into(), format!("http://{}{}{}", hote, chemin, if q.is_empty() { String::new() } else { format!("?{}", q) })));
    }
    // le cookie du launcher reste au launcher
    let biscuits: Vec<String> = vue.cookies().into_iter().filter(|(k, _)| !COOKIES_LAUNCHER.contains(&k.as_str())).map(|(k, v)| format!("{}={}", k, v)).collect();
    if !biscuits.is_empty() {
        sortie.push(("Cookie".into(), biscuits.join("; ")));
    }
    if !tunnel {
        sortie.push(("Accept-Encoding".into(), "identity".into()));
    }
    sortie
}

/// Redirection de l'outil : ses adresses absolues repassent par le relais.
fn location(valeur: &str, id: &str, hote: &str) -> String {
    let (netloc, chemin, requete, fragment) = decouper_url(valeur);
    if !netloc.is_empty() && netloc.to_lowercase() != hote {
        return valeur.into(); // un autre site : inchangé
    }
    if netloc.is_empty() && !valeur.starts_with('/') {
        return valeur.into(); // relative : le navigateur la résout
    }
    let mut c = chemin_outil(if chemin.is_empty() { "/" } else { chemin });
    if !requete.is_empty() {
        c += &format!("?{}", requete);
    }
    if !fragment.is_empty() {
        c += &format!("#{}", fragment);
    }
    format!("{}{}{}", PREFIXE, id, c)
}

/// Set-Cookie de l'outil : sans Domain (il serait celui du Pi, pas du site visité).
/// None : cookie du launcher, refusé.
fn biscuit(valeur: &str) -> Option<String> {
    let nom = valeur.split('=').next().unwrap_or("").trim().to_lowercase();
    let prefixe = nom.starts_with("__host-") || nom.starts_with("__secure-");
    if COOKIES_LAUNCHER.contains(&nom.as_str()) || (prefixe && nom.split_once('-').is_some_and(|(_, n)| COOKIES_LAUNCHER.contains(&n))) {
        return None;
    }
    static DOMAINE: OnceLock<Regex> = OnceLock::new();
    Some(DOMAINE.get_or_init(|| Regex::new(r"(?i);\s*Domain=[^;]*").expect("motif valide")).replace_all(valeur, "").into_owned())
}

/// Liens absolus du HTML (href="/x") : ramenés sous /o/<id>/ ; « //autre.site » est laissé.
fn reecrire_liens(corps: &[u8], id: &str) -> Vec<u8> {
    static LIENS: OnceLock<RegexOctets> = OnceLock::new();
    let liens = LIENS.get_or_init(|| RegexOctets::new(r#"(?i-u)(\b(?:href|src|action|formaction|poster)\s*=\s*)(["'])/"#).expect("motif valide"));
    let racine = prefixe(id);
    let mut sortie = Vec::with_capacity(corps.len() + 1024);
    let mut fin = 0;
    for c in liens.captures_iter(corps) {
        let m = c.get(0).expect("correspondance");
        if corps.get(m.end()) == Some(&b'/') {
            continue;
        }
        sortie.extend_from_slice(&corps[fin..m.start()]);
        sortie.extend_from_slice(&c[1]);
        sortie.extend_from_slice(&c[2]);
        sortie.extend_from_slice(racine.as_bytes());
        fin = m.end();
    }
    sortie.extend_from_slice(&corps[fin..]);
    sortie
}

/// Requête qui agit (envoi, WebSocket) : elle doit partir d'une page de cette adresse.
/// Sec-Fetch-Site est posé par le navigateur lui-même (une page ne peut pas le changer) ; à défaut
/// (vieux navigateur), l'Origin est comparée au nom d'hôte.
pub fn origine_permise(vue: &Vue) -> bool {
    let site = vue.entete("Sec-Fetch-Site").unwrap_or("").trim().to_lowercase();
    if !site.is_empty() {
        return site == "same-origin" || site == "none";
    }
    match vue.entete("Origin") {
        None => true, // pas un envoi depuis une page web
        Some(o) => o != "null" && decouper_url(o).0.to_lowercase() == vue.hote_public(),
    }
}

// -- le serveur du relais -------------------------------------------------------------

pub fn ouvrir(port: u16, app: Arc<App>) -> bool {
    match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(ecoute) => {
            std::thread::spawn(move || servir(ecoute, app));
            true
        }
        Err(e) => {
            println!("  Relais des outils : port {} indisponible ({}), outils non joignables depuis Internet.", port, e);
            false
        }
    }
}

pub fn servir(ecoute: TcpListener, app: Arc<App>) {
    for flux in ecoute.incoming().flatten() {
        let app = app.clone();
        std::thread::spawn(move || {
            let _ = connexion(flux, &app); // le navigateur a coupé : rien à dire
        });
    }
}

fn raison(code: u16) -> &'static str {
    match code {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        411 => "Length Required",
        502 => "Bad Gateway",
        _ => "",
    }
}

/// Une réponse du launcher lui-même (pas de l'outil).
fn repondre(s: &mut TcpStream, vue: &Vue, code: u16, entetes: &[(&str, String)], corps: &[u8]) -> io::Result<()> {
    let mut t = format!("HTTP/1.0 {} {}\r\n", code, raison(code));
    for (k, v) in entetes {
        t += &format!("{}: {}\r\n", k, v);
    }
    t += &format!(
        "Content-Length: {}\r\nCache-Control: no-cache\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n",
        corps.len()
    );
    if vue.https_public() {
        t += "Strict-Transport-Security: max-age=31536000\r\n";
    }
    t += "Connection: close\r\n\r\n";
    s.write_all(t.as_bytes())?;
    s.write_all(corps)?;
    s.flush()
}

fn texte(s: &mut TcpStream, vue: &Vue, code: u16, texte: &str) -> io::Result<()> {
    repondre(s, vue, code, &[("Content-Type", "text/plain; charset=utf-8".into())], texte.as_bytes())
}

fn rediriger(s: &mut TcpStream, vue: &Vue, ou: &str) -> io::Result<()> {
    repondre(s, vue, 302, &[("Location", ou.into())], b"")
}

/// Une ligne d'en-tête (64 Ko au plus pour toute la tête).
fn ligne(lecteur: &mut impl BufRead, total: &mut usize) -> io::Result<String> {
    let mut l = Vec::new();
    lecteur.take((ENTETES_MAX - *total) as u64 + 1).read_until(b'\n', &mut l)?;
    *total += l.len();
    if *total > ENTETES_MAX || !l.ends_with(b"\n") {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(String::from_utf8_lossy(&l).trim_end_matches(['\r', '\n']).to_string())
}

/// La première ligne et les en-têtes.
fn tete(lecteur: &mut impl BufRead) -> io::Result<(String, Vec<(String, String)>)> {
    let mut total = 0;
    let premiere = ligne(lecteur, &mut total)?;
    let mut entetes = Vec::new();
    loop {
        let l = ligne(lecteur, &mut total)?;
        if l.is_empty() {
            return Ok((premiere, entetes));
        }
        if let Some((k, v)) = l.split_once(':') {
            entetes.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
}

fn connexion(flux: TcpStream, app: &App) -> io::Result<()> {
    let attente = Some(Duration::from_secs(30)); // une connexion muette ne garde pas un fil pour toujours
    flux.set_read_timeout(attente)?;
    flux.set_write_timeout(attente)?;
    let client = flux.peer_addr()?;
    let mut lecteur = BufReader::new(flux.try_clone()?);
    let mut sortie = flux;
    let (premiere, entetes) = tete(&mut lecteur)?;
    let mut mots = premiere.split(' ');
    let (Some(methode), Some(chemin)) = (mots.next(), mots.next()) else { return Err(io::ErrorKind::InvalidData.into()) };
    let vue = Vue { client: client.ip(), entetes: &entetes, tls: false };
    if !app.hote_permis(vue.entete("Host")) {
        return texte(&mut sortie, &vue, 403, "Nom d'hôte non autorisé");
    }
    let Some((id, chemin)) = cible(chemin, &vue) else {
        return texte(&mut sortie, &vue, 404, "Outil introuvable : ouvrir les outils depuis le launcher.");
    };
    if app.connecte(&vue).is_none() {
        if SANS_EFFET[..2].contains(&methode) && vue.entete("Accept").unwrap_or("").contains("text/html") {
            let hote = vue.hote_public();
            let hote = hote.split(':').next().unwrap_or("");
            let ou = if vue.https() { format!("https://{}/connexion.html", hote) } else { "/connexion.html".into() };
            return rediriger(&mut sortie, &vue, &ou);
        }
        return texte(&mut sortie, &vue, 401, "Connexion requise.");
    }
    let port = app.g.outil(&id).map(|o| o.port()).or_else(|| app.g.port_sous_outil(&id));
    match port {
        None => texte(&mut sortie, &vue, 404, "Outil inconnu."),
        Some(_) if chemin.is_empty() => rediriger(&mut sortie, &vue, &prefixe(&id)),
        Some(port) => relayer(&mut sortie, lecteur, &vue, methode, port, &id, &chemin),
    }
}

/// Transmet la requête à l'outil et renvoie sa réponse.
fn relayer(sortie: &mut TcpStream, lecteur: BufReader<TcpStream>, vue: &Vue, methode: &str, port: u16, id: &str, chemin: &str) -> io::Result<()> {
    let hote = format!("127.0.0.1:{}", port);
    let chemin = chemin_outil(chemin);
    let tunnel = vue.entete("Upgrade").unwrap_or("").eq_ignore_ascii_case("websocket");
    if (tunnel || !SANS_EFFET.contains(&methode)) && !origine_permise(vue) {
        return texte(sortie, vue, 403, "Requête refusée (autre site).");
    }
    if vue.entete("Service-Worker").unwrap_or("").eq_ignore_ascii_case("script") {
        return texte(sortie, vue, 403, "Service worker refusé à travers le launcher.");
    }
    let mut envoye = false;
    let r = if tunnel {
        tunnel_ws(sortie, lecteur, vue, methode, port, &hote, &chemin, &mut envoye)
    } else {
        requete(sortie, lecteur, vue, methode, port, &hote, id, &chemin, &mut envoye)
    };
    if r.is_err() && !envoye {
        return texte(sortie, vue, 502, "L'outil ne répond pas (est-il démarré ?).");
    }
    r
}

fn amont(port: u16, delai: Duration) -> io::Result<TcpStream> {
    let s = TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port), Duration::from_secs(10))?;
    s.set_read_timeout(Some(delai))?;
    s.set_write_timeout(Some(delai))?;
    Ok(s)
}

#[allow(clippy::too_many_arguments)]
fn requete(
    sortie: &mut TcpStream,
    mut lecteur: BufReader<TcpStream>,
    vue: &Vue,
    methode: &str,
    port: u16,
    hote: &str,
    id: &str,
    chemin: &str,
    envoye: &mut bool,
) -> io::Result<()> {
    if vue.entete("Transfer-Encoding").unwrap_or("").to_lowercase().contains("chunked") {
        *envoye = true;
        return texte(sortie, vue, 411, "Envoi en morceaux non pris en charge.");
    }
    let mut longueur = None;
    if !["GET", "HEAD", "OPTIONS", "DELETE"].contains(&methode) || vue.entete("Content-Length").is_some_and(|v| !v.is_empty()) {
        match vue.entete("Content-Length").unwrap_or("").trim() {
            "" => longueur = Some(0),
            v => match v.parse::<u64>() {
                Ok(n) => longueur = Some(n),
                Err(_) => {
                    *envoye = true;
                    return texte(sortie, vue, 400, "Requête illisible.");
                }
            },
        }
    }
    let mut a = amont(port, Duration::from_secs(300))?;
    // HTTP/1.0 : l'outil répond avec une longueur, ou ferme à la fin (jamais en morceaux)
    let mut demande = format!("{} {} HTTP/1.0\r\n", methode, chemin);
    for (k, v) in entetes_vers_outil(vue, hote, false) {
        demande += &format!("{}: {}\r\n", k, v);
    }
    if let Some(n) = longueur {
        demande += &format!("Content-Length: {}\r\n", n);
    }
    demande += "\r\n";
    a.write_all(demande.as_bytes())?;
    let mut restant = longueur.unwrap_or(0);
    let mut tampon = vec![0u8; 65536];
    while restant > 0 {
        let n = lecteur.read(&mut tampon[..restant.min(65536) as usize])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "client coupé"));
        }
        a.write_all(&tampon[..n])?;
        restant -= n as u64;
    }
    let mut rep = BufReader::new(a);
    let (premiere, entetes) = tete(&mut rep)?;
    let mut mots = premiere.splitn(3, ' ');
    let _version = mots.next();
    let code: u16 = mots.next().and_then(|c| c.parse().ok()).ok_or(io::ErrorKind::InvalidData)?;
    let raison_outil = mots.next().unwrap_or("").to_string();
    let entete = |nom: &str| entetes.iter().find(|(k, _)| k.eq_ignore_ascii_case(nom)).map(|(_, v)| v.clone());
    let taille = entete("Content-Length");
    let sans_corps = methode == "HEAD" || code == 204 || code == 304;
    let html = entete("Content-Type").unwrap_or_default().to_lowercase().starts_with("text/html")
        && !sans_corps
        && taille.as_ref().map(|t| t.parse::<u64>().is_ok_and(|n| n <= HTML_MAX)).unwrap_or(true);
    let limite = taille.as_ref().and_then(|t| t.parse::<u64>().ok());
    let mut corps = None;
    let mut taille_envoyee = taille.clone();
    if html {
        let mut v = Vec::new();
        (&mut rep).take(limite.unwrap_or(HTML_MAX + 1).min(HTML_MAX + 1)).read_to_end(&mut v)?;
        if v.len() as u64 > HTML_MAX {
            *envoye = true;
            return texte(sortie, vue, 502, "Page trop grande pour le relais."); // trop gros pour être réécrit
        }
        let v = reecrire_liens(&v, id);
        taille_envoyee = Some(v.len().to_string());
        corps = Some(v);
    }
    let mut t = format!("HTTP/1.0 {} {}\r\n", code, raison_outil);
    for (k, v) in &entetes {
        let bas = k.to_lowercase();
        if SAUF_REPONSE.contains(&bas.as_str()) {
            continue;
        }
        let v = match bas.as_str() {
            "location" => location(v, id, hote),
            "set-cookie" => match biscuit(v) {
                Some(b) => b,
                None => continue,
            },
            _ => v.clone(),
        };
        t += &format!("{}: {}\r\n", k, v);
    }
    if let Some(n) = &taille_envoyee {
        t += &format!("Content-Length: {}\r\n", n);
    }
    t += &format!("Referrer-Policy: same-origin\r\nSet-Cookie: {}={}{}\r\n\r\n", COOKIE_OUTIL, id, vue.options_cookie());
    *envoye = true;
    sortie.write_all(t.as_bytes())?;
    if sans_corps {
        return sortie.flush();
    }
    if let Some(c) = corps {
        sortie.write_all(&c)?;
        return sortie.flush();
    }
    // flux (SSE...) : chaque morceau part aussitôt
    let mut source: Box<dyn Read> = match limite {
        Some(n) => Box::new(rep.take(n)),
        None => Box::new(rep),
    };
    loop {
        let n = source.read(&mut tampon)?;
        if n == 0 {
            break;
        }
        sortie.write_all(&tampon[..n])?;
        sortie.flush()?;
    }
    Ok(())
}

fn secondes() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Copie dans un sens jusqu'à la fermeture, ou ATTENTE_TUNNEL s sans rien dans aucun des deux sens.
fn pomper(mut de: TcpStream, mut vers: TcpStream, vu: Arc<AtomicU64>) {
    let mut tampon = vec![0u8; 65536];
    loop {
        match de.read(&mut tampon) {
            Ok(0) => break,
            Ok(n) => {
                if vers.write_all(&tampon[..n]).is_err() {
                    break;
                }
                vu.store(secondes(), Ordering::SeqCst);
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                if secondes().saturating_sub(vu.load(Ordering::SeqCst)) < ATTENTE_TUNNEL {
                    continue;
                }
                break;
            }
            Err(_) => break,
        }
    }
    let _ = de.shutdown(Shutdown::Both);
    let _ = vers.shutdown(Shutdown::Both);
}

/// WebSocket : la demande part telle quelle (Host et Origin ramenés à l'outil), puis les octets
/// passent dans les deux sens jusqu'à la fermeture.
#[allow(clippy::too_many_arguments)]
fn tunnel_ws(
    sortie: &mut TcpStream,
    lecteur: BufReader<TcpStream>,
    vue: &Vue,
    methode: &str,
    port: u16,
    hote: &str,
    chemin: &str,
    envoye: &mut bool,
) -> io::Result<()> {
    let veille = Duration::from_secs(60); // réveil régulier pour compter l'inactivité
    let mut a = amont(port, veille)?;
    let mut demande = format!("{} {} HTTP/1.1\r\n", methode, chemin);
    for (k, v) in entetes_vers_outil(vue, hote, true) {
        demande += &format!("{}: {}\r\n", k, v);
    }
    demande += "\r\n";
    a.write_all(demande.as_bytes())?;
    *envoye = true;
    let deja = lecteur.buffer().to_vec(); // ce que le navigateur a déjà envoyé après la demande
    if !deja.is_empty() {
        a.write_all(&deja)?;
    }
    let client = lecteur.into_inner();
    client.set_read_timeout(Some(veille))?;
    sortie.set_write_timeout(Some(Duration::from_secs(ATTENTE_TUNNEL)))?;
    let vu = Arc::new(AtomicU64::new(secondes()));
    let (a2, s2, vu2) = (a.try_clone()?, sortie.try_clone()?, vu.clone());
    let retour = std::thread::spawn(move || pomper(a2, s2, vu2));
    pomper(client, a, vu);
    let _ = retour.join();
    Ok(())
}

#[cfg(test)]
mod essais {
    use super::*;

    fn vue(entetes: &[(&str, &str)]) -> (Vec<(String, String)>, IpAddr) {
        (entetes.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), "127.0.0.1".parse().unwrap())
    }

    #[test]
    fn urls() {
        assert_eq!(decouper_url("http://h:1/a/b?x=1#f"), ("h:1", "/a/b", "x=1", "f"));
        assert_eq!(decouper_url("//ext.example/x"), ("ext.example", "/x", "", ""));
        assert_eq!(decouper_url("/o/faux/?a"), ("", "/o/faux/", "a", ""));
        assert_eq!(decouper_url("null"), ("", "null", "", ""));
    }

    #[test]
    fn cibles() {
        let (e, ip) = vue(&[]);
        let v = Vue { client: ip, entetes: &e, tls: false };
        assert_eq!(cible("/o/faux/page?x", &v), Some(("faux".into(), "/page?x".into())));
        assert_eq!(cible("/o/faux", &v), Some(("faux".into(), "".into())));
        assert_eq!(cible("/o/faux?x=1", &v), Some(("faux".into(), "/?x=1".into())));
        assert_eq!(cible("/o/", &v), None);
        assert_eq!(cible("/page", &v), None);
        let (e, ip) = vue(&[("Referer", "http://autre.example/o/faux/sous/x"), ("Cookie", "relais_outil=autre")]);
        let v = Vue { client: ip, entetes: &e, tls: false };
        assert_eq!(cible("/page", &v), Some(("faux".into(), "/page".into())));
        let (e, ip) = vue(&[("Cookie", "relais_outil=faux")]);
        let v = Vue { client: ip, entetes: &e, tls: false };
        assert_eq!(cible("/ws", &v), Some(("faux".into(), "/ws".into())));
    }

    #[test]
    fn liens_redirections_cookies() {
        let html = br#"<a href="/page">a</a><img src='/img.png'><a href="//ext.example/x">b</a><a href="relatif">c</a><form action="/envoyer">"#;
        let r = String::from_utf8(reecrire_liens(html, "faux")).unwrap();
        assert!(r.contains(r#"href="/o/faux/page""#));
        assert!(r.contains("src='/o/faux/img.png'"));
        assert!(r.contains(r#"action="/o/faux/envoyer""#));
        assert!(r.contains(r#"href="//ext.example/x""#));
        assert!(r.contains(r#"href="relatif""#));
        assert_eq!(location("http://127.0.0.1:9/page", "faux", "127.0.0.1:9"), "/o/faux/page");
        assert_eq!(location("https://ailleurs.fr/x", "faux", "127.0.0.1:9"), "https://ailleurs.fr/x");
        assert_eq!(location("relatif", "faux", "127.0.0.1:9"), "relatif");
        assert_eq!(location("/a?b#c", "faux", "h"), "/o/faux/a?b#c");
        assert_eq!(biscuit("a=1; Domain=exemple.fr; Path=/").as_deref(), Some("a=1; Path=/"));
        assert_eq!(biscuit("launcher_session=pirate; Path=/"), None);
        assert_eq!(biscuit("__Host-launcher_session=x; Path=/; Secure"), None);
        assert_eq!(biscuit("relais_outil=x"), None);
    }

    #[test]
    fn origines() {
        let caddy = [("X-Forwarded-For", "203.0.113.7, 127.0.0.1"), ("X-Forwarded-Host", "pi.tail1234.ts.net"), ("X-Forwarded-Proto", "https")];
        for (origine, attendu) in [("https://evil.example", false), ("http://127.0.0.1:8080", false), ("null", false), ("https://pi.tail1234.ts.net", true)] {
            let mut l = caddy.to_vec();
            l.push(("Origin", origine));
            let (e, ip) = vue(&l);
            assert_eq!(origine_permise(&Vue { client: ip, entetes: &e, tls: false }), attendu, "{origine}");
        }
        for (site, attendu) in [("same-origin", true), ("none", true), ("same-site", false), ("cross-site", false)] {
            let (e, ip) = vue(&[("Sec-Fetch-Site", site), ("Origin", "https://evil.example")]);
            assert_eq!(origine_permise(&Vue { client: ip, entetes: &e, tls: false }), attendu, "{site}");
        }
    }
}
