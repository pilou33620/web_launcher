//! Un outil ouvert à travers le launcher (/o/<id>/) : connexion, liens, redirections, WebSocket.
mod commun;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use commun::*;
use launcher::serveur::{App, Vue};
use serde_json::json;

const PUBLIC: &str = "pi.tail1234.ts.net";
const CADDY: [(&str, &str); 4] =
    [("X-Forwarded-For", "203.0.113.7, 127.0.0.1"), ("X-Forwarded-Host", PUBLIC), ("X-Forwarded-Proto", "https"), ("Tailscale-Funnel-Request", "?1")];

type VuWs = Arc<Mutex<Option<(String, String, String)>>>;

/// Le faux outil : une page HTML, des redirections, des cookies, un flux, un écho POST et WebSocket.
fn faux_outil(vu_ws: VuWs) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let vu = vu_ws.clone();
            std::thread::spawn(move || {
                let _ = servir_outil(s, vu);
            });
        }
    });
    port
}

fn servir_outil(mut s: TcpStream, vu_ws: VuWs) -> std::io::Result<()> {
    let mut l = BufReader::new(s.try_clone()?);
    let mut premiere = String::new();
    l.read_line(&mut premiere)?;
    let mut mots = premiere.split_whitespace();
    let (methode, chemin) = (mots.next().unwrap_or("").to_string(), mots.next().unwrap_or("").to_string());
    let mut entetes: Vec<(String, String)> = Vec::new();
    loop {
        let mut x = String::new();
        l.read_line(&mut x)?;
        let x = x.trim_end();
        if x.is_empty() {
            break;
        }
        if let Some((k, v)) = x.split_once(':') {
            entetes.push((k.trim().to_lowercase(), v.trim().to_string()));
        }
    }
    let h = |k: &str| entetes.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let repondre = |s: &mut TcpStream, code: u16, type_: &str, corps: &[u8], autres: &[(&str, &str)]| -> std::io::Result<()> {
        let mut t = format!("HTTP/1.0 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\n", code, type_, corps.len());
        for (k, v) in autres {
            t += &format!("{}: {}\r\n", k, v);
        }
        t += "\r\n";
        s.write_all(t.as_bytes())?;
        s.write_all(corps)
    };
    if h("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
        s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")?;
        *vu_ws.lock().unwrap() = Some((chemin, h("host").unwrap_or_default(), h("origin").unwrap_or_default()));
        let mut tampon = [0u8; 1024];
        loop {
            let n = l.read(&mut tampon)?;
            if n == 0 {
                return Ok(());
            }
            s.write_all(&tampon[..n].to_ascii_uppercase())?;
        }
    }
    if methode == "POST" {
        let n: usize = h("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        let mut corps = vec![0u8; n];
        l.read_exact(&mut corps)?;
        let somme: u64 = corps.iter().map(|b| *b as u64).sum::<u64>() % 65536;
        let vu = json!({"Host": h("host"), "Origin": h("origin"), "Referer": h("referer"), "Cookie": h("cookie"),
                        "Authorization": h("authorization"), "taille": n, "somme": somme});
        return repondre(&mut s, 200, "application/json", vu.to_string().as_bytes(), &[]);
    }
    match chemin.as_str() {
        "/" => repondre(&mut s, 200, "text/html; charset=utf-8",
            br#"<a href="/page">a</a><img src='/img.png'><a href="//ext.example/x">b</a><a href="relatif">c</a><form action="/envoyer">"#, &[]),
        "/page" => repondre(&mut s, 200, "text/plain", format!("page:{}", h("host").unwrap_or_default()).as_bytes(), &[]),
        "/redir" => {
            let ou = format!("http://{}/page", h("host").unwrap_or_default());
            repondre(&mut s, 302, "text/plain", b"", &[("Location", ou.as_str())])
        }
        "/mauvais_cookie" => repondre(&mut s, 200, "text/plain", b"ok", &[("Set-Cookie", "launcher_session=pirate; Path=/"),
            ("Set-Cookie", "__Host-launcher_session=x; Path=/; Secure"), ("Service-Worker-Allowed", "/"), ("Clear-Site-Data", "\"*\"")]),
        "/cookie" => repondre(&mut s, 200, "text/plain", b"ok", &[("Set-Cookie", "a=1; Domain=exemple.fr; Path=/")]),
        "/flux" => {
            // sans Content-Length : fermé à la fin
            s.write_all(b"HTTP/1.0 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: 1\n\n")?;
            std::thread::sleep(Duration::from_millis(100));
            s.write_all(b"data: 2\n\n")
        }
        _ => repondre(&mut s, 404, "text/plain", b"introuvable", &[]),
    }
}

struct Banc {
    port: u16,
    port_outil: u16,
    jeton: String,
    vu_ws: VuWs,
    app: Arc<App>,
}

fn banc(nom: &str) -> Banc {
    let d = dossier(nom);
    let vu_ws: VuWs = Arc::new(Mutex::new(None));
    let port_outil = faux_outil(vu_ws.clone());
    let morts = port_libre();
    let parent_arrete = port_libre();
    let app = app(
        &d,
        json!({"outils": [
            {"id": "faux", "nom": "Faux", "port": port_outil, "commande": ["x"]},
            {"id": "mort", "nom": "Mort", "port": morts, "commande": ["x"]},
            {"id": "parent", "nom": "Parent", "port": parent_arrete, "commande": ["x"], "relais_sous_outils": {"sous": port_outil}},
        ]}),
    );
    assert_eq!(app.gardien.ajouter("pilou", "un-mot-de-passe-long"), None);
    let jeton = app.gardien.verifier("pilou", "un-mot-de-passe-long", "127.0.0.1").unwrap();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let a = app.clone();
    std::thread::spawn(move || launcher::relais::servir(l, a));
    Banc { port, port_outil, jeton: format!("launcher_session={}", jeton), vu_ws, app }
}

impl Banc {
    fn get(&self, chemin: &str, connecte: bool, autres: &[(&str, &str)]) -> Rep {
        self.demander("GET", chemin, b"", connecte, autres)
    }
    fn demander(&self, methode: &str, chemin: &str, corps: &[u8], connecte: bool, autres: &[(&str, &str)]) -> Rep {
        let mut e: Vec<(&str, &str)> = Vec::new();
        if connecte && !autres.iter().any(|(k, _)| *k == "Cookie") {
            e.push(("Cookie", self.jeton.as_str()));
        }
        e.extend_from_slice(autres);
        demander(self.port, methode, chemin, &e, corps)
    }
    fn hote(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

#[test]
fn connexion_exigee() {
    let b = banc("rconnexion");
    let r = b.get("/o/faux/", false, &[("Accept", "text/html")]);
    assert_eq!((r.code, r.entete("Location")), (302, Some("/connexion.html")));
    assert_eq!(b.get("/o/faux/page", false, &[]).code, 401);
    assert_eq!(b.demander("POST", "/o/faux/echo", b"x", false, &[]).code, 401);
    let referer = format!("http://{}/o/faux/", b.hote());
    assert_eq!(b.get("/page", false, &[("Referer", referer.as_str())]).code, 401); // le Referer d'un outil ne donne aucun droit
}

#[test]
fn sous_outils() {
    let b = banc("rsous");
    assert_eq!(b.get("/o/sous/", true, &[]).code, 200);
    assert_eq!(b.get("/o/inconnu/", true, &[]).code, 404);
    assert_eq!(b.get("/o/sous/", false, &[]).code, 401);
    assert_eq!(b.app.g.port_sous_outil("faux"), None);
}

#[test]
fn page_liens_et_redirections() {
    let b = banc("rpage");
    let r = b.get("/o/faux/", true, &[]);
    assert_eq!(r.code, 200);
    let t = r.texte();
    for attendu in [r#"href="/o/faux/page""#, "src='/o/faux/img.png'", r#"action="/o/faux/envoyer""#, r#"href="//ext.example/x""#, r#"href="relatif""#] {
        assert!(t.contains(attendu), "{attendu} : {t}");
    }
    assert_eq!(r.entete("Content-Length").unwrap().parse::<usize>().unwrap(), r.corps.len());
    assert_eq!(r.entete("Referrer-Policy"), Some("same-origin"));
    assert!(r.entete("Set-Cookie").unwrap().contains("relais_outil=faux"));
    assert_eq!(b.get("/o/faux/page", true, &[]).texte(), format!("page:127.0.0.1:{}", b.port_outil));
    let r = b.get("/o/faux", true, &[]);
    assert_eq!((r.code, r.entete("Location")), (302, Some("/o/faux/")));
    assert_eq!(b.get("/o/absent/", true, &[]).code, 404);
    assert_eq!(b.get("/o/mort/", true, &[]).code, 502);
    let r = b.get("/o/faux/inconnu", true, &[]);
    assert_eq!((r.code, r.texte().as_str()), (404, "introuvable")); // le 404 de l'outil, tel quel
    let r = b.get("/o/faux/redir", true, &[]);
    assert_eq!((r.code, r.entete("Location")), (302, Some("/o/faux/page")));
    let cookies = b.get("/o/faux/cookie", true, &[]).tous("Set-Cookie");
    assert_eq!(cookies[0], "a=1; Path=/");
    assert!(cookies[1].starts_with("relais_outil=faux;"));
}

#[test]
fn referer_pour_les_liens_absolus() {
    let b = banc("rreferer");
    let page = format!("page:127.0.0.1:{}", b.port_outil);
    assert_eq!(b.get("/page", true, &[("Referer", format!("http://{}/o/faux/", b.hote()).as_str())]).texte(), page);
    assert_eq!(b.get("/page", true, &[("Referer", format!("http://{}/o/faux/sous/x", b.hote()).as_str())]).code, 200);
    assert_eq!(b.get("/page", true, &[("Referer", "http://autre.example/o/faux/")]).code, 200); // chemin seul compte
    assert_eq!(b.get("/page", true, &[]).code, 404); // aucun outil désigné
    let avec = format!("{}; relais_outil=faux", b.jeton);
    assert_eq!(b.get("/page", true, &[("Cookie", avec.as_str())]).code, 200);
    assert_eq!(b.get("/page", false, &[("Cookie", "relais_outil=faux")]).code, 401);
    // le relais ne sert que les outils : jamais l'API ni les pages du launcher
    assert_eq!(b.get("/api/etat", true, &[]).code, 404);
    assert_eq!(b.demander("POST", "/api/comptes/ajouter", b"{}", true, &[("X-Launcher", "1")]).code, 404);
    assert_eq!(b.get("/index.html", true, &[]).code, 404);
}

#[test]
fn envoi_nettoye() {
    let b = banc("renvoi");
    let gros: Vec<u8> = (0..1000).flat_map(|_| 0..=255u8).collect(); // 256 000 octets
    let biscuits = format!("{}; theme=sombre", b.jeton);
    let origine = format!("https://{}", PUBLIC);
    let referer = format!("https://{}/o/faux/dossier/page?x=1", PUBLIC);
    let mut e = vec![("Origin", origine.as_str()), ("Authorization", "Basic eHg6eHg="), ("Referer", referer.as_str()), ("Cookie", biscuits.as_str())];
    e.extend_from_slice(&CADDY);
    let r = b.demander("POST", "/o/faux/echo", &gros, true, &e);
    assert_eq!(r.code, 200);
    let vu = r.json();
    let somme: u64 = gros.iter().map(|x| *x as u64).sum::<u64>() % 65536;
    assert_eq!((vu["taille"].as_u64(), vu["somme"].as_u64()), (Some(gros.len() as u64), Some(somme)));
    let outil = format!("127.0.0.1:{}", b.port_outil);
    assert_eq!(vu["Host"], outil.as_str());
    // un outil en --local (fausse ici sur 127.0.0.1) contrôle toujours le Host : il accepte celui du relais
    assert!(web_tools_commun::appairage::hote_permis(Some(&outil)));
    assert_eq!(vu["Origin"], format!("http://{}", outil).as_str());
    assert_eq!(vu["Referer"], format!("http://{}/dossier/page?x=1", outil).as_str());
    assert_eq!(vu["Cookie"], "theme=sombre"); // ni session du launcher, ni rien d'autre
    assert!(vu["Authorization"].is_null());
}

#[test]
fn origine_etrangere_refusee() {
    let b = banc("rorigine");
    for origine in ["https://evil.example", "http://127.0.0.1:8080", "null"] {
        let mut e = vec![("Origin", origine)];
        e.extend_from_slice(&CADDY);
        assert_eq!(b.demander("POST", "/o/faux/echo", b"x", true, &e).code, 403, "{origine}");
    }
    let local = format!("http://{}", b.hote());
    assert_eq!(b.demander("POST", "/o/faux/echo", b"x", true, &[("Origin", local.as_str())]).code, 200); // réseau local
    assert_eq!(b.get("/o/faux/page", true, &[("Origin", "https://evil.example")]).code, 200); // lecture : la session Strict suffit
    let origine = format!("https://{}:8443", PUBLIC);
    let mut mal: Vec<(&str, &str)> = CADDY.iter().filter(|(k, _)| *k != "X-Forwarded-Host").copied().collect();
    mal.push(("X-Forwarded-Host", "127.0.0.1:18081"));
    mal.extend([("Origin", origine.as_str()), ("Sec-Fetch-Site", "same-origin")]);
    assert_eq!(b.demander("POST", "/o/faux/echo", b"x", true, &mal).code, 200); // Sec-Fetch-Site prime
    for site in ["same-site", "cross-site"] {
        let mut e = vec![("Origin", origine.as_str()), ("Sec-Fetch-Site", site)];
        e.extend_from_slice(&CADDY);
        assert_eq!(b.demander("POST", "/o/faux/echo", b"x", true, &e).code, 403, "{site}");
    }
}

#[test]
fn outil_ne_touche_pas_au_launcher() {
    let b = banc("rintouchable");
    assert_eq!(b.get("/o/faux/sw.js", true, &[("Service-Worker", "script")]).code, 403);
    let r = b.get("/o/faux/mauvais_cookie", true, &[]);
    let noms: Vec<String> = r.tous("Set-Cookie").iter().map(|c| c.split('=').next().unwrap().to_string()).collect();
    assert_eq!(noms, vec!["relais_outil"]); // launcher_session et __Host- refusés
    assert!(r.entete("Service-Worker-Allowed").is_none());
    assert!(r.entete("Clear-Site-Data").is_none());
}

#[test]
fn derriere_caddy() {
    let b = banc("rcaddy");
    let referer = format!("https://{}/o/faux/", PUBLIC);
    let mut e = vec![("Referer", referer.as_str()), ("Host", "localhost")];
    e.extend_from_slice(&CADDY);
    assert_eq!(b.get("/page", true, &e).code, 200);
    let r = b.get("/o/faux/", true, &[&[("Host", "localhost")][..], &CADDY[..]].concat());
    assert!(r.entete("Set-Cookie").unwrap().contains("Secure"));
    assert!(r.entete("Strict-Transport-Security").is_none()); // celui de l'outil n'est jamais transmis
    assert!(!b.get("/o/faux/", true, &[]).entete("Set-Cookie").unwrap().contains("Secure"));
    // vers la connexion, sur le nom public
    let mut e: Vec<(&str, &str)> = CADDY.iter().filter(|(k, _)| *k != "X-Forwarded-Host").copied().collect();
    let hote = format!("{}:8443", PUBLIC);
    e.extend([("X-Forwarded-Host", hote.as_str()), ("Accept", "text/html"), ("Host", "localhost")]);
    let r = b.get("/o/faux/", false, &e);
    assert_eq!((r.code, r.entete("Location")), (302, Some(format!("https://{}/connexion.html", PUBLIC).as_str())));
    assert_eq!(r.entete("Strict-Transport-Security"), Some("max-age=31536000"));
}

#[test]
fn ip_du_visiteur() {
    let cas: [(&str, &[(&str, &str)], &str); 7] = [
        ("192.168.1.5", &[], "192.168.1.5"),
        ("127.0.0.1", &[], "127.0.0.1"),
        ("127.0.0.1", &[("X-Forwarded-For", "203.0.113.7, 127.0.0.1")], "203.0.113.7"),
        ("127.0.0.1", &[("X-Forwarded-For", "6.6.6.6, 203.0.113.7")], "203.0.113.7"), // la plus à droite
        ("127.0.0.1", &[("X-Forwarded-For", "127.0.0.1")], "internet"),
        ("127.0.0.1", &[("Tailscale-Funnel-Request", "?1")], "internet"),
        ("192.168.1.5", &[("X-Forwarded-For", "6.6.6.6")], "192.168.1.5"), // pas par Caddy : ignoré
    ];
    for (pair, entetes, attendu) in cas {
        let e: Vec<(String, String)> = entetes.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let vue = Vue { client: pair.parse().unwrap(), entetes: &e, tls: false };
        assert_eq!(vue.ip_client(), attendu, "{pair} {entetes:?}");
    }
}

#[test]
fn flux_sans_longueur() {
    let b = banc("rflux");
    let r = b.get("/o/faux/flux", true, &[]);
    assert_eq!((r.code, r.corps.as_slice()), (200, &b"data: 1\n\ndata: 2\n\n"[..]));
    assert_eq!(r.entete("Content-Type"), Some("text/event-stream"));
}

fn websocket(b: &Banc, chemin: &str, autres: &[(&str, &str)]) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", b.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let origine = format!("https://{}", PUBLIC);
    let mut h: Vec<(&str, &str)> = vec![("Host", "localhost"), ("Upgrade", "websocket"), ("Connection", "Upgrade"), ("Sec-WebSocket-Key", "abc"),
                                        ("Sec-WebSocket-Version", "13"), ("Origin", origine.as_str())];
    h.extend_from_slice(&CADDY);
    for (k, v) in autres {
        h.retain(|(n, _)| n != k);
        h.push((*k, *v));
    }
    let t: String = h.iter().map(|(k, v)| format!("{}: {}\r\n", k, v)).collect();
    s.write_all(format!("GET {} HTTP/1.1\r\n{}\r\n", chemin, t).as_bytes()).unwrap();
    s
}

fn lire(s: &mut TcpStream, jusqua: &[u8]) -> Vec<u8> {
    let mut d = Vec::new();
    let mut t = [0u8; 4096];
    while !d.windows(jusqua.len()).any(|w| w == jusqua) {
        match s.read(&mut t) {
            Ok(n) if n > 0 => d.extend_from_slice(&t[..n]),
            _ => break,
        }
    }
    d
}

#[test]
fn websocket_dans_les_deux_sens() {
    let b = banc("rws");
    let mut s = websocket(&b, "/o/faux/ws", &[("Cookie", b.jeton.as_str())]);
    assert!(String::from_utf8_lossy(&lire(&mut s, b"\r\n\r\n")).starts_with("HTTP/1.1 101"));
    s.write_all(b"bonjour").unwrap();
    assert_eq!(lire(&mut s, b"BONJOUR"), b"BONJOUR");
    let outil = format!("127.0.0.1:{}", b.port_outil);
    assert_eq!(b.vu_ws.lock().unwrap().clone(), Some(("/ws".to_string(), outil.clone(), format!("http://{}", outil))));
    // chemin absolu /ws : l'outil est retrouvé par le cookie du relais
    let avec = format!("{}; relais_outil=faux", b.jeton);
    let mut s2 = websocket(&b, "/ws", &[("Cookie", avec.as_str())]);
    assert!(String::from_utf8_lossy(&lire(&mut s2, b"\r\n\r\n")).starts_with("HTTP/1.1 101"));
    s2.write_all(b"ok").unwrap();
    assert_eq!(lire(&mut s2, b"OK"), b"OK");
}

#[test]
fn websocket_refuse() {
    let b = banc("rwsrefus");
    let mut s = websocket(&b, "/o/faux/ws", &[]);
    assert!(String::from_utf8_lossy(&lire(&mut s, b"\r\n")).contains(" 401 "));
    let mut s = websocket(&b, "/o/faux/ws", &[("Cookie", b.jeton.as_str()), ("Origin", "https://evil.example")]);
    assert!(String::from_utf8_lossy(&lire(&mut s, b"\r\n")).contains(" 403 "));
}
