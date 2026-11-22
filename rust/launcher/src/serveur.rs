//! Serveur web du launcher : la page (web/) et l'API JSON utilisée par la page, puis le lancement.
//!
//! HTTP et HTTPS sur le même port (serveur de web_tools_commun) : un autre appareil arrivé en HTTP
//! est renvoyé vers https:// pour que le mot de passe ne passe pas en clair sur le Wi-Fi.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::IpAddr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Map, Value};
use web_tools_commun::appairage::{self, deballer};
use web_tools_commun::serveur::{decoder_chemin, ErreurCorps, Reponse, Requete, Serveur};

use crate::andy::Andy;
use crate::claude::{self, Claude};
use crate::compte::{self, Gardien};
use crate::config::{self, Chemins};
use crate::gestionnaire::{Erreur, Gestionnaire};
use crate::reseau::{self, AliasMdns};
use crate::{depot, maintenant, tenir, texte, vrai};

pub const COOKIE: &str = "launcher_session";
/// Ouvert sans connexion : la page de connexion et ce qu'elle charge.
const PUBLIC: [&str; 2] = ["/connexion.html", "/js/connexion.js"];
/// Posés par Caddy ou Tailscale Funnel : une requête qui les porte vient d'Internet, pas de ce poste.
pub const ENTETES_PROXY: [&str; 6] = ["X-Forwarded-For", "X-Forwarded-Host", "X-Forwarded-Proto", "Forwarded", "X-Real-IP", "Tailscale-Funnel-Request"];
const CORPS_MAX: usize = 8192;
const LOT_CONSOLE: u64 = 64 * 1024;
/// Affichés par le launcher, en lecture seule.
const DOSSIERS_VUS: [(&str, &str); 2] = [("/api/temp", "temp"), ("/api/securite", "rpi_securite")];
const CSP_TEMP: &str = "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:";
pub const APERCU_TEMP: u64 = 1024 * 1024;
const LISTE_TEMP: usize = 500;

/// WEB_TOOLS_BANC_DISTANT=1 (bancs d'essai) : ce poste est traité comme un autre appareil.
/// La variable ne peut que retirer des droits.
fn banc_distant() -> bool {
    std::env::var("WEB_TOOLS_BANC_DISTANT").as_deref() == Ok("1")
}

/// Ce que le serveur sait d'une requête pour décider des droits (serveur principal et relais).
pub struct Vue<'a> {
    pub client: IpAddr,
    pub entetes: &'a [(String, String)],
    /// Connexion chiffrée par le launcher lui-même (Wi-Fi).
    pub tls: bool,
}

impl<'a> Vue<'a> {
    pub fn de(r: &'a Requete) -> Vue<'a> {
        Vue { client: r.client.ip(), entetes: &r.entetes, tls: r.https }
    }

    pub fn entete(&self, nom: &str) -> Option<&str> {
        self.entetes.iter().find(|(k, _)| k.eq_ignore_ascii_case(nom)).map(|(_, v)| v.as_str())
    }

    /// La connexion TCP vient de la machine elle-même (le Pi, ou Caddy devant le launcher).
    pub fn pair_local(&self) -> bool {
        !banc_distant() && deballer(self.client).is_loopback()
    }

    pub fn par_proxy(&self) -> bool {
        self.pair_local() && ENTETES_PROXY.iter().any(|k| self.entete(k).is_some_and(|v| !v.is_empty()))
    }

    /// Quelqu'un assis devant le Pi : connexion locale SANS passer par Caddy/Funnel (derrière eux,
    /// toutes les requêtes d'Internet arrivent aussi de 127.0.0.1).
    pub fn ce_poste(&self) -> bool {
        self.pair_local() && !self.par_proxy()
    }

    /// L'adresse du visiteur. Derrière Caddy/Funnel : la dernière adresse non locale de
    /// X-Forwarded-For (posée par Funnel, qui efface celle envoyée par le visiteur).
    pub fn ip_client(&self) -> String {
        if !self.par_proxy() {
            return deballer(self.client).to_string();
        }
        for morceau in self.entete("X-Forwarded-For").unwrap_or("").split(',').rev() {
            if let Some(a) = appairage::lire_ip(morceau.trim()) {
                if !a.is_loopback() {
                    return a.to_string();
                }
            }
        }
        "internet".into() // adresse inconnue : tous les visiteurs d'Internet comptent ensemble
    }

    pub fn https_public(&self) -> bool {
        self.par_proxy() && self.entete("X-Forwarded-Proto").unwrap_or("").trim().eq_ignore_ascii_case("https")
    }

    pub fn https(&self) -> bool {
        self.tls || self.https_public()
    }

    /// Le nom tapé par le visiteur (Caddy remplace Host par « localhost »).
    pub fn hote_public(&self) -> String {
        if let Some(h) = self.entete("X-Forwarded-Host").filter(|h| self.par_proxy() && !h.is_empty()) {
            return h.split(',').next().unwrap_or("").trim().to_lowercase();
        }
        self.entete("Host").unwrap_or("").trim().to_lowercase()
    }

    pub fn options_cookie(&self) -> String {
        format!("; Path=/; HttpOnly; SameSite=Strict{}", if self.https() { "; Secure" } else { "" })
    }

    /// Les cookies reçus, dans l'ordre.
    pub fn cookies(&self) -> Vec<(String, String)> {
        let Some(c) = self.entete("Cookie") else { return Vec::new() };
        c.split(';')
            .filter_map(|m| {
                let (k, v) = m.trim().split_once('=')?;
                Some((k.trim().to_string(), v.trim().trim_matches('"').to_string()))
            })
            .collect()
    }

    pub fn cookie(&self, nom: &str) -> Option<String> {
        self.cookies().into_iter().find(|(k, _)| k == nom).map(|(_, v)| v)
    }
}

/// Les appareils qui utilisent ce serveur, affichés dans son terminal : une ligne quand une IP
/// arrive, ou revient après INACTIF secondes sans requête (même format que les outils).
pub struct Visites {
    nom: String,
    vues: Mutex<HashMap<String, Instant>>,
}

impl Visites {
    const INACTIF: Duration = Duration::from_secs(600);

    pub fn new(nom: &str) -> Visites {
        Visites { nom: nom.into(), vues: Mutex::new(HashMap::new()) }
    }

    pub fn noter(&self, ip: &str, qui: Option<&str>) {
        let mut ip = appairage::lire_ip(ip).map(|a| a.to_string()).unwrap_or_else(|| ip.to_string());
        if let Some(q) = qui {
            ip = format!("{} ({})", ip, q);
        }
        let actifs = {
            let mut vues = tenir(&self.vues);
            let arrivee = vues.get(&ip).map(|t| t.elapsed() > Self::INACTIF).unwrap_or(true);
            vues.retain(|_, t| t.elapsed() <= Self::INACTIF);
            vues.insert(ip.clone(), Instant::now());
            if !arrivee {
                return;
            }
            let mut a: Vec<String> = vues.keys().cloned().collect();
            a.sort();
            a
        };
        self.dire(&ip, &format!("{} appareil(s) actif(s) : {}", actifs.len(), actifs.join(", ")));
    }

    pub fn dire(&self, ip: &str, texte: &str) {
        println!("  [connexion] {}  {:<11} {}  ->  {}", crate::heure("%H:%M:%S"), self.nom, ip, texte);
        let _ = std::io::stdout().flush();
    }
}

pub struct App {
    pub g: Arc<Gestionnaire>,
    pub gardien: Arc<Gardien>,
    pub andy: Andy,
    pub claude: Claude,
    pub alias: Option<Arc<AliasMdns>>,
    /// port, nom_mdns, port_public_relais, nom_machine, dossier_outils, demarre.
    pub infos: Map<String, Value>,
    pub visites: Visites,
    pub web: PathBuf,
    /// Redémarre le launcher (après sa mise à jour, ou sur demande).
    pub relancer: Arc<dyn Fn() + Send + Sync>,
}

impl App {
    fn dossier_outils(&self) -> PathBuf {
        PathBuf::from(self.infos.get("dossier_outils").and_then(Value::as_str).unwrap_or("."))
    }

    /// Parade au DNS rebinding : le nom demandé doit désigner ce Raspberry Pi (nom de la machine,
    /// son alias mDNS, son nom Tailscale exact, ou une adresse IP).
    pub fn hote_permis(&self, entete: Option<&str>) -> bool {
        let h = entete.unwrap_or("").trim().to_lowercase();
        let mut h = h.trim_end_matches('.');
        if let Some(reste) = h.strip_prefix('[') {
            h = reste.split(']').next().unwrap_or("");
        } else if h.matches(':').count() == 1 {
            h = h.split(':').next().unwrap_or("");
        }
        let nom = reseau::nom_machine();
        let alias = self.infos.get("nom_mdns").and_then(Value::as_str).unwrap_or("").to_lowercase();
        let locaux = [nom.clone(), format!("{}.local", nom), alias.clone(), format!("{}.local", alias)];
        if locaux.iter().any(|n| n == h && !n.starts_with('.') && !n.is_empty()) {
            return true;
        }
        appairage::hote_permis(entete)
    }

    /// Some(identifiant ou None) : la requête est admise. Sans compte créé, seul ce poste entre (la
    /// page le signale en rouge) : un autre appareil ne peut ni prendre la main ni créer le premier compte.
    pub fn connecte(&self, vue: &Vue) -> Option<Option<String>> {
        let login = self.gardien.utilisateur(vue.cookie(COOKIE).as_deref());
        if login.is_none() && (self.gardien.actif() || !vue.ce_poste()) {
            return None;
        }
        self.visites.noter(&vue.ip_client(), login.as_deref());
        Some(login)
    }
}

/// En-têtes de toutes les réponses du launcher (pas celles des outils relayés).
/// X-Frame-Options : SAMEORIGIN, posé par le serveur commun ; « frame-ancestors 'none' » garde
/// l'ancien DENY partout sauf pour les aperçus HTML et PDF de Temp.
pub fn envelopper(rep: Reponse, vue: &Vue, meme_origine: bool) -> Reponse {
    let mut rep = rep.entete("X-Content-Type-Options", "nosniff").entete("Referrer-Policy", "no-referrer");
    if !meme_origine {
        rep = rep.entete("Content-Security-Policy", "frame-ancestors 'none'");
    }
    if vue.https_public() {
        // pas sur le Wi-Fi : son certificat n'a de sens qu'avec la CA du poste
        rep = rep.entete("Strict-Transport-Security", "max-age=31536000");
    }
    rep
}

fn erreur(code: u16, texte: &str) -> Reponse {
    Reponse::json(code, &json!({ "erreur": texte }))
}

fn ok() -> Reponse {
    Reponse::json(200, &json!({"ok": true}))
}

/// Le corps en objet JSON (vide : {}), ou None.
fn objet(corps: &[u8]) -> Option<Map<String, Value>> {
    if corps.is_empty() {
        return Some(Map::new());
    }
    match serde_json::from_slice(corps) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    }
}

pub fn servir(r: &mut Requete, app: &App) -> Reponse {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| servir_brut(r, app))).unwrap_or_else(|_| erreur(500, "erreur interne du launcher"))
}

fn servir_brut(r: &mut Requete, app: &App) -> Reponse {
    if !app.hote_permis(r.entete("Host")) {
        return envelopper(Reponse::texte(403, "Nom d'hôte non autorisé"), &Vue::de(r), false);
    }
    if r.tls_actif {
        // le port 80 est implicite en http, pas en https : il est ajouté pour le renvoi
        let mut hote = r.entete("Host").map(str::to_string).unwrap_or_else(|| deballer(r.local.ip()).to_string());
        if !hote.rsplit(']').next().unwrap_or("").contains(':') {
            hote = format!("{}:{}", hote, r.local.port());
        }
        r.entetes.retain(|(k, _)| !k.eq_ignore_ascii_case("Host"));
        r.entetes.push(("Host".into(), hote));
        if let Some(rep) = appairage::filtrer_tls(r) {
            return envelopper(rep, &Vue::de(r), false);
        }
    }
    match r.methode.as_str() {
        "GET" => get(r, app),
        "POST" => post(r, app),
        _ => envelopper(Reponse::texte(405, "Méthode non permise."), &Vue::de(r), false),
    }
}

/// « /css/../index.html » : refusé, comme tout chemin que la normalisation changerait.
fn chemin_normal(brut: &str) -> bool {
    if brut == "/" {
        return true;
    }
    let t = brut.trim_end_matches('/');
    brut.starts_with('/') && !t.is_empty() && t.split('/').skip(1).all(|m| !m.is_empty() && m != "." && m != "..")
}

fn type_web(p: &Path) -> Option<&'static str> {
    Some(match p.extension()?.to_str()? {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        _ => return None,
    })
}

/// Un fichier de web/ ; rien hors de ce dossier, ni fichier caché, ni extension inconnue.
fn fichier_web(app: &App, chemin: &str) -> Reponse {
    let rel = if chemin == "/" { "index.html".to_string() } else { decoder_chemin(chemin.trim_start_matches('/')) };
    let p = Path::new(&rel);
    let sur = p.components().all(|c| matches!(c, Component::Normal(n) if !n.to_string_lossy().starts_with('.')));
    let f = app.web.join(p);
    match (sur, type_web(&f), f.is_file()) {
        (true, Some(t), true) => match std::fs::read(&f) {
            Ok(corps) => Reponse { code: 200, entetes: vec![("Content-Type".into(), t.into()), ("Cache-Control".into(), "no-cache".into())], corps, suite: None },
            Err(_) => Reponse::texte(404, "introuvable"),
        },
        _ => Reponse::texte(404, "introuvable"),
    }
}

fn get(r: &Requete, app: &App) -> Reponse {
    let vue = Vue::de(r);
    let rep = get_brut(r, app, &vue);
    // les aperçus HTML et PDF de Temp s'affichent dans un cadre de la page
    let cadre = rep.entetes.iter().any(|(k, v)| k == "X-Apercu-Cadre" && v == "1");
    let mut rep = envelopper(rep, &vue, cadre);
    rep.entetes.retain(|(k, _)| k != "X-Apercu-Cadre");
    rep
}

fn get_brut(r: &Requete, app: &App, vue: &Vue) -> Reponse {
    let chemin = r.chemin().to_string();
    let brut = decoder_chemin(&chemin);
    if brut.contains('\\') || brut.contains('\0') || !chemin_normal(&brut) {
        return Reponse::texte(404, "introuvable");
    }
    if PUBLIC.contains(&chemin.as_str()) || chemin.starts_with("/css/") || chemin.starts_with("/icones/") {
        if chemin == "/connexion.html" && app.gardien.actif() && app.connecte(vue).is_some() {
            return Reponse::redirection(302, "/");
        }
        return fichier_web(app, &chemin);
    }
    let Some(login) = app.connecte(vue) else {
        if chemin.starts_with("/api/") {
            return erreur(401, "connexion requise");
        }
        return Reponse::redirection(302, "/connexion.html");
    };
    let g = &app.g;
    if chemin == "/api/etat" {
        return Reponse::json(200, &etat(app, r, login));
    }
    if chemin == "/api/claude" {
        return Reponse::json(200, &app.claude.etat());
    }
    for (prefixe, journal) in [("/api/journal/", true), ("/api/console/", false)] {
        if chemin.starts_with(prefixe) {
            let Some(o) = g.outil(chemin.rsplit('/').next().unwrap_or("")) else { return erreur(404, "outil inconnu") };
            if journal {
                let lignes = r.param("lignes").and_then(|l| l.trim().parse().ok()).unwrap_or(300);
                return Reponse::texte(200, &lire_fin(&o.journal(), lignes));
            }
            let depuis = r.param("depuis").and_then(|l| l.trim().parse().ok()).unwrap_or(-1);
            let (position, texte) = lire_depuis(&o.journal(), depuis);
            let (pid, phase, attend) = {
                let e = o.e();
                (e.pid, e.phase.clone(), e.attend_saisie)
            };
            return Reponse::json(
                200,
                &json!({"position": position, "texte": texte, "pid": pid, "phase": phase,
                        "attend_saisie": attend, "terminal": o.avec_terminal()}),
            );
        }
    }
    for (route, dossier) in DOSSIERS_VUS {
        if chemin == route {
            let racine = app.dossier_outils().join(dossier);
            let mut fichiers = lister_temp(&racine);
            let total = fichiers.len();
            fichiers.truncate(LISTE_TEMP);
            return Reponse::json(200, &json!({"dossier": racine.to_string_lossy(), "total": total, "fichiers": fichiers}));
        }
        if chemin.strip_suffix("/fichier") == Some(route) {
            return fichier_temp(app, dossier, &r.param("chemin").unwrap_or_default(), r.param("apercu").is_some());
        }
    }
    match chemin.as_str() {
        "/api/andy/donnees" => {
            return match app.andy.historique() {
                Some(d) => Reponse::json(200, &d),
                None => erreur(404, "historique absent : andy_donnees.local.json sur le Pi"),
            }
        }
        "/api/andy/pesees" => return Reponse::json(200, &json!({ "pesees": app.andy.lire() })),
        "/api/andy/evenements" => return Reponse::json(200, &json!({ "evenements": app.andy.lire_evenements() })),
        _ => {}
    }
    if chemin.starts_with("/api/") {
        return erreur(404, "route inconnue");
    }
    let permis = ["/", "/index.html", "/console.html", "/andy.html"].contains(&chemin.as_str())
        || ["/css/", "/js/", "/icones/"].iter().any(|p| chemin.starts_with(p));
    if !permis {
        return Reponse::texte(404, "introuvable");
    }
    fichier_web(app, &chemin)
}

fn etat(app: &App, r: &Requete, login: Option<String>) -> Value {
    let g = &app.g;
    let mut infos = app.infos.clone();
    let ip = reseau::ip_locale();
    infos.insert("ip".into(), json!(ip));
    // l'adresse que cet appareil a jointe (réseau local ou Tailscale) : les outils s'ouvrent dessus
    let ici = deballer(r.local.ip()).to_string();
    infos.insert("ici".into(), if ici.starts_with("127.") { Value::Null } else { json!(ici) });
    let port = infos.get("port").and_then(Value::as_u64).unwrap_or(80);
    let suffixe = if port == 80 { String::new() } else { format!(":{}", port) };
    let mut adresses = Vec::new();
    if let Some(ip) = &ip {
        adresses.push(json!({"type": "Réseau", "url": format!("http://{}{}/", ip, suffixe)}));
    }
    if let Some(ts) = reseau::ip_tailscale() {
        adresses.push(json!({"type": "Tailscale", "url": format!("http://{}{}/", ts, suffixe)}));
    }
    let mdns = app.alias.as_ref().map(|a| a.etat());
    if let Some(nom) = infos.get("nom_mdns").and_then(Value::as_str).filter(|n| !n.is_empty()) {
        if matches!(mdns.as_deref(), Some("annoncé") | Some("nom de la machine")) {
            adresses.push(json!({"type": "Nom", "url": format!("http://{}.local{}/", nom, suffixe)}));
        }
    }
    adresses.push(json!({"type": "Machine", "url": format!("http://{}.local{}/", reseau::nom_machine(), suffixe)}));
    let masques: Vec<Value> = g
        .masques()
        .iter()
        .map(|c| json!({"id": c.get("id"), "nom": c.get("nom"), "suffixe": c.get("suffixe").cloned().unwrap_or(json!("")), "port": c.get("port")}))
        .collect();
    infos.insert("adresses".into(), json!(adresses));
    infos.insert("compte".into(), json!(app.gardien.actif()));
    infos.insert("utilisateur".into(), json!(login));
    infos.insert("comptes".into(), json!(app.gardien.comptes()));
    infos.insert("masques".into(), json!(masques));
    infos.insert("mdns".into(), json!(mdns.unwrap_or_else(|| "désactivé".into())));
    infos.insert("version".into(), depot::version(&g.chemins.racine));
    infos.insert("maj".into(), tenir(&g.launcher_maj).json());
    infos.insert("action".into(), json!(*tenir(&g.launcher_action)));
    infos.insert("heure".into(), json!(maintenant() as i64));
    infos.insert("heures_maj".into(), json!(g.heures_maj));
    infos.insert("prochaine_verif".into(), json!(*tenir(&g.prochaine_verif)));
    json!({"launcher": infos, "outils": g.etat()})
}

fn post(r: &mut Requete, app: &App) -> Reponse {
    let rep = post_brut(r, app);
    envelopper(rep, &Vue::de(r), false)
}

fn post_brut(r: &mut Requete, app: &App) -> Reponse {
    // En-tête propre au launcher : un autre site ne peut pas l'envoyer sans autorisation CORS
    // (que le launcher ne donne jamais).
    if r.entete("X-Launcher") != Some("1") {
        return erreur(403, "requête refusée");
    }
    let corps = match r.lire_corps(CORPS_MAX) {
        Ok(c) => c,
        Err(ErreurCorps::TropGrand) => return erreur(413, "requête trop grande"),
        Err(_) => return erreur(400, "requête illisible"),
    };
    let chemin = r.chemin().trim_matches('/').to_string();
    let m: Vec<&str> = chemin.split('/').collect();
    let vue = Vue::de(r);
    if m == ["api", "connexion"] {
        return connexion(app, &vue, &corps);
    }
    if m == ["api", "deconnexion"] {
        app.gardien.fermer(vue.cookie(COOKIE).as_deref());
        return ok().entete("Set-Cookie", &format!("{}=; Max-Age=0{}", COOKIE, vue.options_cookie()));
    }
    let Some(login) = app.connecte(&vue) else { return erreur(401, "connexion requise") };
    let qui = format!("{} ({})", vue.ip_client(), login.as_deref().unwrap_or("sans compte"));
    match post_connecte(app, &vue, &m, &corps, &qui) {
        Ok(rep) => rep,
        Err(Erreur::Inconnu) => erreur(404, "outil ou action inconnu"),
        Err(Erreur::Occupe(texte)) => erreur(409, &texte),
        Err(Erreur::Valeur(texte)) => erreur(400, &texte),
    }
}

fn post_connecte(app: &App, vue: &Vue, m: &[&str], corps: &[u8], qui: &str) -> Result<Reponse, Erreur> {
    let g = &app.g;
    match m {
        ["api", "console", id] => {
            let Some(d) = objet(corps) else { return Ok(erreur(400, "saisie illisible")) };
            let saisie = filtrer_saisie(&texte(d.get("texte"), "", 100_000));
            if saisie.is_empty() {
                return Ok(erreur(400, "saisie vide"));
            }
            match g.envoyer(id, &saisie) {
                Err(Erreur::Valeur(t)) => Ok(erreur(409, &t)),
                Err(e) => Err(e),
                Ok(()) => Ok(ok()),
            }
        }
        ["api", "andy", genre @ ("pesees" | "evenements"), reste @ ..] if reste.len() <= 1 => {
            Ok(pesee(app, reste.first().copied().filter(|a| !a.is_empty()).unwrap_or("ajouter"), corps, genre))
        }
        ["api", "claude", action @ ("activer" | "redemarrer" | "eteindre" | "allumer" | "confiance")] => Ok(actions_claude(app, action, corps, qui)),
        ["api", "comptes", action] => Ok(comptes(app, vue, action, corps)),
        ["api", "outils", id, "reglages"] => {
            let Some(d) = objet(corps) else { return Ok(erreur(400, "réglages illisibles")) };
            let present = |k: &str| d.get(k).filter(|v| !v.is_null());
            g.regler(id, present("port"), present("affiche"))?;
            Ok(ok())
        }
        ["api", "outils", id, "maj_auto"] => {
            let Some(d) = objet(corps) else { return Ok(erreur(400, "demande illisible")) };
            g.regler_maj_auto(id, d.get("valeur").map(vrai).unwrap_or(false))?;
            Ok(ok())
        }
        ["api", "outils", id, action] => {
            g.lancer_action(id, action)?;
            app.visites.dire(qui, &format!("{} {}", action, id));
            Ok(ok())
        }
        ["api", "ajouter_outil"] => {
            let Some(d) = objet(corps) else { return Ok(erreur(400, "demande illisible")) };
            let url = texte(d.get("depot"), "", 2000);
            let options = if d.get("options").is_some_and(vrai) { texte(d.get("options"), "", 2000) } else { String::new() };
            match g.ajouter(&url, &options) {
                Ok(oid) => {
                    app.visites.dire(qui, &format!("ajout {}", oid));
                    Ok(Reponse::json(200, &json!({"ok": true, "id": oid})))
                }
                Err(t) => Ok(erreur(400, &t)),
            }
        }
        ["api", "verifier_tout"] => {
            for o in g.outils() {
                if o.installe() {
                    let _ = g.lancer_action(&o.id, "verifier"); // occupé : il sera vu plus tard
                }
            }
            let g2 = g.clone();
            std::thread::spawn(move || {
                g2.verifier_launcher();
            });
            Ok(ok())
        }
        ["api", "launcher", "verifier"] => Ok(Reponse::json(200, &json!({"ok": true, "maj": g.verifier_launcher().json()}))),
        ["api", "launcher", "mettre_a_jour"] => Ok(maj_launcher(app)),
        ["api", "launcher", "redemarrer"] => {
            {
                let mut a = tenir(&g.launcher_action);
                if a.is_some() {
                    return Ok(erreur(409, "déjà en cours"));
                }
                *a = Some("Redémarrage".into());
            }
            relancer_bientot(app);
            Ok(Reponse::json(200, &json!({"ok": true, "message": "Redémarrage du launcher…"})))
        }
        _ => Ok(erreur(404, "route inconnue")),
    }
}

/// La réponse part d'abord : le launcher redémarre 0,8 s plus tard.
fn relancer_bientot(app: &App) {
    let relancer = app.relancer.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(800));
        relancer();
    });
}

fn connexion(app: &App, vue: &Vue, corps: &[u8]) -> Reponse {
    let Some(d) = objet(corps) else { return erreur(400, "demande illisible") };
    let (login, mdp) = (texte(d.get("login"), "", 200), texte(d.get("mot_de_passe"), "", 1000));
    let ip = vue.ip_client();
    match app.gardien.verifier(&login, &mdp, &ip) {
        Err(e) => {
            // l'identifiant refusé n'est pas affiché : texte venu du réseau, pas dans le terminal
            app.visites.dire(&ip, &format!("échec de connexion ({})", e));
            erreur(if e.starts_with("Trop") { 429 } else { 401 }, &e)
        }
        Ok(jeton) => {
            app.visites.dire(&ip, &format!("connexion de {}", login));
            ok().entete("Set-Cookie", &format!("{}={}; Max-Age={}{}", COOKIE, jeton, compte::DUREE_SESSION, vue.options_cookie()))
        }
    }
}

/// Ajouter un compte, changer un mot de passe, supprimer un compte (tous ont les mêmes droits).
fn comptes(app: &App, vue: &Vue, action: &str, corps: &[u8]) -> Reponse {
    let Some(d) = objet(corps) else { return erreur(400, "demande illisible") };
    let (login, mdp) = (texte(d.get("login"), "", 200), texte(d.get("mot_de_passe"), "", 1000));
    let resultat = match action {
        "ajouter" => app.gardien.ajouter(&login, &mdp),
        "mot_de_passe" => app.gardien.changer_mdp(&login, &mdp, vue.cookie(COOKIE).as_deref()),
        "supprimer" => app.gardien.supprimer(&login),
        _ => return erreur(404, "route inconnue"),
    };
    match resultat {
        Some(e) => erreur(400, &e),
        None => ok(),
    }
}

/// Suivi d'Andy : ajouter ou supprimer une pesée ou un évènement.
fn pesee(app: &App, action: &str, corps: &[u8], genre: &str) -> Reponse {
    let Some(d) = objet(corps) else { return erreur(400, "demande illisible") };
    let champ = |k: &str| d.get(k).cloned().unwrap_or(Value::Null);
    match action {
        "ajouter" => {
            let r = if genre == "evenements" {
                let note = d.get("note").cloned().unwrap_or(json!(""));
                app.andy.ajouter_evenement(&champ("date"), &champ("type"), &champ("titre"), &note, None).map(|e| json!({"ok": true, "evenement": e}))
            } else {
                app.andy.ajouter(&champ("date"), &champ("poids"), None).map(|p| json!({"ok": true, "pesee": p}))
            };
            match r {
                Ok(v) => Reponse::json(200, &v),
                Err(e) => erreur(400, &e),
            }
        }
        "supprimer" => {
            if app.andy.supprimer(&texte(d.get("id"), "", 40)) {
                ok()
            } else {
                erreur(404, "entrée introuvable")
            }
        }
        _ => erreur(404, "route inconnue"),
    }
}

/// Sessions Claude Code : ajouter/retirer un dossier, redémarrer/éteindre/allumer un dossier ou tout.
fn actions_claude(app: &App, action: &str, corps: &[u8], qui: &str) -> Reponse {
    let Some(d) = objet(corps) else { return erreur(400, "demande illisible") };
    let chemin = d.get("chemin").filter(|v| !v.is_null()).map(|v| texte(Some(v), "", 300));
    let c = &app.claude;
    let r = match action {
        "activer" => c.activer(chemin.as_deref(), d.get("actif").map(vrai).unwrap_or(false)),
        "confiance" => c.faire_confiance(chemin.as_deref()),
        "redemarrer" => c.redemarrer(chemin.as_deref()),
        "eteindre" => c.eteindre(chemin.as_deref()),
        _ => c.allumer(chemin.as_deref()),
    };
    match r {
        Err(claude::Erreur::Valeur(e)) => erreur(400, &e),
        Err(claude::Erreur::Autre(e)) => erreur(500, &e.chars().take(300).collect::<String>()),
        Ok(()) => {
            let cible = chemin.filter(|c| !c.is_empty()).unwrap_or_else(|| "tout".into());
            app.visites.dire(qui, &format!("claude {} {}", action, cible));
            ok()
        }
    }
}

fn maj_launcher(app: &App) -> Reponse {
    let g = &app.g;
    {
        let mut a = tenir(&g.launcher_action);
        if a.is_some() {
            return erreur(409, "déjà en cours");
        }
        *a = Some("Mise à jour".into());
    }
    match g.mettre_a_jour_launcher() {
        Err(e) => {
            *tenir(&g.launcher_action) = None;
            erreur(500, &e)
        }
        Ok(false) => {
            *tenir(&g.launcher_action) = None;
            Reponse::json(200, &json!({"ok": true, "message": "Déjà à jour."}))
        }
        Ok(true) => {
            *tenir(&g.launcher_action) = Some("Redémarrage".into());
            relancer_bientot(app);
            Reponse::json(200, &json!({"ok": true, "message": "Mis à jour, redémarrage du launcher…"}))
        }
    }
}

// -- Temp et Sécurité ---------------------------------------------------------------

/// Types affichés dans le navigateur : les autres fichiers sont du texte brut ou un téléchargement.
// shortcut: pas de requêtes Range, Safari iOS ne lit pas ces vidéos ; à ajouter si le besoin vient
fn type_temp(chemin: &Path) -> Option<&'static str> {
    let ext = chemin.extension()?.to_str()?.to_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "html" | "htm" => "text/html; charset=utf-8",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        _ => return None,
    })
}

/// image, video, audio, pdf, html ; sinon le contenu décide : texte ou autre.
fn genre_temp(chemin: &Path) -> std::io::Result<&'static str> {
    if let Some(t) = type_temp(chemin) {
        return Ok(match t.split('/').next().unwrap_or("") {
            "application" => "pdf",
            "text" => "html",
            "image" => "image",
            "video" => "video",
            _ => "audio",
        });
    }
    let mut debut = Vec::new();
    File::open(chemin)?.take(4096).read_to_end(&mut debut)?;
    Ok(if debut.contains(&0) { "autre" } else { "texte" })
}

/// Les fichiers de temp/, sous-dossiers compris, les plus récents d'abord (ni cachés ni liens).
pub fn lister_temp(racine: &Path) -> Vec<Value> {
    let mut fichiers: Vec<(i64, String, Value)> = Vec::new();
    let mut pile = vec![racine.to_path_buf()];
    while let Some(d) = pile.pop() {
        let Ok(lecture) = std::fs::read_dir(&d) else { continue };
        for e in lecture.flatten() {
            // nom illisible : il casserait toute la réponse JSON
            let Some(nom) = e.file_name().to_str().map(str::to_string) else { continue };
            let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
            if nom.starts_with('.') {
                continue;
            }
            if meta.is_dir() {
                pile.push(e.path());
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let Some(relatif) = e.path().strip_prefix(racine).ok().and_then(|p| p.to_str().map(str::to_string)) else { continue };
            let Ok(genre) = genre_temp(&e.path()) else { continue };
            let date = meta.mtime();
            fichiers.push((date, relatif.clone(), json!({"chemin": relatif, "taille": meta.len(), "date": date, "genre": genre})));
        }
    }
    fichiers.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    fichiers.into_iter().map(|f| f.2).collect()
}

/// Le fichier demandé dans temp/, ou None : ni sortie du dossier, ni fichier caché, ni lien.
pub fn chemin_temp(racine: &Path, relatif: &str) -> Option<PathBuf> {
    if relatif.contains('\0') || relatif.split('/').any(|m| m.is_empty() || m.starts_with('.')) {
        return None;
    }
    let racine = std::fs::canonicalize(racine).ok()?;
    let cible = racine.join(relatif);
    (std::fs::canonicalize(&cible).ok()? == cible && cible.is_file()).then_some(cible)
}

/// urllib.parse.quote (sauf « / »).
fn quote(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) { (b as char).to_string() } else { format!("%{:02X}", b) })
        .collect()
}

/// Un fichier de temp/ ou de rpi_securite/. Il vient d'une IA, donc rien n'y tourne sur l'origine
/// du launcher : HTML et SVG sont bloqués par CSP, le reste part en texte brut ou en téléchargement.
fn fichier_temp(app: &App, dossier: &str, relatif: &str, apercu: bool) -> Reponse {
    let introuvable = || erreur(404, "fichier introuvable");
    let Some(cible) = chemin_temp(&app.dossier_outils().join(dossier), relatif) else { return introuvable() };
    let Ok(genre) = genre_temp(&cible) else { return introuvable() };
    let ouvert = std::fs::OpenOptions::new().read(true).custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits()).open(&cible);
    let Ok(mut f) = ouvert else { return introuvable() };
    let Ok(taille) = f.metadata().map(|m| m.len()) else { return introuvable() };
    if apercu {
        let mut debut = Vec::new();
        if (&mut f).take(APERCU_TEMP).read_to_end(&mut debut).is_err() {
            return introuvable();
        }
        let mut texte = String::from_utf8_lossy(&debut).into_owned();
        if taille > APERCU_TEMP {
            texte += &format!("\n\n[… suite non affichée : fichier de {} octets]", taille);
        }
        return Reponse::texte(200, &texte);
    }
    let type_ = type_temp(&cible).unwrap_or(if genre == "texte" { "text/plain; charset=utf-8" } else { "application/octet-stream" });
    let mut rep = Reponse::flux(200, type_, taille, f);
    if genre == "html" || type_ == "image/svg+xml" {
        rep = rep.entete("Content-Security-Policy", CSP_TEMP);
    }
    if genre == "autre" {
        let nom = cible.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        rep = rep.entete("Content-Disposition", &format!("attachment; filename*=UTF-8''{}", quote(&nom)));
    }
    if genre == "html" || genre == "pdf" {
        rep = rep.entete("X-Apercu-Cadre", "1");
    }
    rep
}

// -- terminaux et journaux ----------------------------------------------------------

/// Le jeton réseau de WEB·SUITE ouvre l'accès sans code d'appairage : jamais affiché.
pub fn masquer(texte: &str) -> String {
    static JETON: OnceLock<Regex> = OnceLock::new();
    JETON.get_or_init(|| Regex::new(r"jeton=[\w-]+").expect("motif valide")).replace_all(texte, "jeton=•••").into_owned()
}

/// Texte, Entrée, Tab, effacement et Ctrl+C / Ctrl+D seulement : pas de Ctrl+Z (suspendre
/// l'outil) ni de Ctrl+\, ni de séquences d'échappement.
pub fn filtrer_saisie(texte: &str) -> String {
    texte.chars().filter(|c| ['\r', '\n', '\t', '\x03', '\x04', '\x7f'].contains(c) || *c >= ' ').take(2000).collect()
}

/// (position, texte) : ce que le journal a reçu depuis la position (en octets).
/// depuis < 0, ou journal renouvelé : les derniers 64 Ko.
pub fn lire_depuis(chemin: &Path, depuis: i64) -> (u64, String) {
    let lire = || -> std::io::Result<(u64, Vec<u8>)> {
        let mut f = File::open(chemin)?;
        let taille = f.seek(SeekFrom::End(0))?;
        let depuis = if depuis < 0 || depuis as u64 > taille { taille.saturating_sub(LOT_CONSOLE) } else { depuis as u64 };
        f.seek(SeekFrom::Start(depuis))?;
        let mut donnees = Vec::new();
        f.take((taille - depuis).min(LOT_CONSOLE)).read_to_end(&mut donnees)?;
        Ok((depuis, donnees))
    };
    let Ok((depuis, mut donnees)) = lire() else { return (0, String::new()) };
    // pas de caractère UTF-8 coupé en deux : la fin incomplète attend le lot suivant
    let n = donnees.len();
    for recul in 1..=n.min(4) {
        let octet = donnees[n - recul];
        if octet & 0xC0 != 0x80 {
            let besoin = if octet >= 0xF0 { 4 } else if octet >= 0xE0 { 3 } else if octet >= 0xC0 { 2 } else { 1 };
            if besoin > recul {
                donnees.truncate(n - recul);
            }
            break;
        }
    }
    // un jeton pas encore écrit en entier attend aussi : il ne doit jamais sortir à moitié
    let debut_ligne = donnees.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
    let trouve = |v: &[u8]| v.windows(6).rposition(|w| w == b"jeton=");
    if trouve(&donnees[debut_ligne..]).is_some() {
        let coupe = if debut_ligne > 0 { debut_ligne } else { trouve(&donnees).unwrap_or(0) };
        donnees.truncate(coupe);
    }
    (depuis + donnees.len() as u64, masquer(&String::from_utf8_lossy(&donnees)))
}

pub fn lire_fin(chemin: &Path, lignes: i64) -> String {
    let lire = || -> std::io::Result<Vec<u8>> {
        let mut f = File::open(chemin)?;
        let taille = f.seek(SeekFrom::End(0))?;
        f.seek(SeekFrom::Start(taille.saturating_sub(200 * 1024)))?;
        let mut v = Vec::new();
        f.read_to_end(&mut v)?;
        Ok(v)
    };
    let Ok(octets) = lire() else { return "(journal vide)".into() };
    let texte = String::from_utf8_lossy(&octets).replace("\r\n", "\n");
    let mut l: Vec<&str> = texte.split(['\n', '\r']).collect();
    if texte.ends_with(['\n', '\r']) {
        l.pop();
    }
    let n = lignes.clamp(1, 3000) as usize;
    masquer(&l[l.len().saturating_sub(n)..].join("\n"))
}

// -- lancement ------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub port: Option<u16>,
    pub dossier: Option<String>,
    pub sans_mdns: bool,
    pub compte: bool,
}

pub const USAGE: &str = "usage: launcher [--port PORT] [--dossier DOSSIER] [--sans-mdns] [--compte]\n\n\
Web Launcher : les web tools du Raspberry Pi.\n  \
--port PORT        port d'écoute (défaut : outils.json, 80)\n  \
--dossier DOSSIER  dossier des web tools (défaut : ~/Documents)\n  \
--sans-mdns        ne pas annoncer launcher.local\n  \
--compte           ajouter un compte ou changer un mot de passe, puis quitter";

pub fn lire_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut i = 0;
    while i < argv.len() {
        let x = argv[i].as_str();
        let (cle, val) = match x.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (x, None),
        };
        let mut valeur = |nom: &str| -> Result<String, String> {
            match &val {
                Some(v) => Ok(v.clone()),
                None => {
                    i += 1;
                    argv.get(i).cloned().ok_or(format!("{} attend une valeur", nom))
                }
            }
        };
        match cle {
            "--port" => {
                let v = valeur("--port")?;
                a.port = Some(v.parse().map_err(|_| format!("port invalide : {}", v))?);
            }
            "--dossier" => a.dossier = Some(valeur("--dossier")?),
            "--sans-mdns" if val.is_none() => a.sans_mdns = true,
            "--compte" if val.is_none() => a.compte = true,
            _ => return Err(format!("option inconnue : {}", x)),
        }
        i += 1;
    }
    Ok(a)
}

fn afficher(texte: &str) {
    println!("{}", texte);
    let _ = std::io::stdout().flush();
}

/// Une ligne au clavier ; `secret` : sans écho (mot de passe). Fin de l'entrée : on quitte.
fn lire_ligne(invite: &str, secret: bool) -> String {
    print!("{}", invite);
    let _ = std::io::stdout().flush();
    let ancien = if secret { nix::sys::termios::tcgetattr(std::io::stdin()).ok() } else { None };
    if let Some(t) = &ancien {
        let mut s = t.clone();
        s.local_flags.remove(nix::sys::termios::LocalFlags::ECHO);
        let _ = nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, &s);
    }
    let mut ligne = String::new();
    let lu = std::io::stdin().read_line(&mut ligne);
    if let Some(t) = &ancien {
        let _ = nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, t);
        println!();
    }
    if !matches!(lu, Ok(n) if n > 0) {
        println!();
        std::process::exit(1);
    }
    ligne.trim_end_matches(['\r', '\n']).to_string()
}

/// Ouvre le port voulu (80 pour « http://launcher.local » sans numéro), sinon le port de secours.
fn ouvrir(port: u16, secours: Option<u16>) -> Arc<Serveur> {
    for p in [Some(port), secours].into_iter().flatten() {
        match Serveur::ouvrir(p, true, 1) {
            Ok(s) => return s,
            Err(_) => match std::net::TcpListener::bind(("0.0.0.0", p)) {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => afficher(&format!(
                    "  Port {} refusé (il faut le service systemd du launcher, voir README) ; essai du port {}.",
                    p,
                    secours.map(|s| s.to_string()).unwrap_or_else(|| "None".into())
                )),
                Err(e) => afficher(&format!("  Port {} indisponible ({}).", p, e)),
                Ok(_) => afficher(&format!("  Port {} indisponible.", p)),
            },
        }
    }
    eprintln!("Impossible d'ouvrir un port pour le launcher.");
    std::process::exit(1);
}

/// Remplace le processus par l'exécutable du launcher (même PID : systemd et les outils, ses
/// enfants, ne voient rien ; il les retrouve en repartant).
fn relancer_processus(exe: &Path) -> ! {
    let _ = std::io::stdout().flush();
    let erreur = std::process::Command::new(exe).args(std::env::args_os().skip(1)).exec();
    eprintln!("Redémarrage du launcher impossible : {}", erreur);
    std::process::exit(1); // systemd le relance (Restart=always)
}

pub fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("--console") {
        std::process::exit(crate::console::main(&argv[1..]));
    }
    if argv.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", USAGE);
        return;
    }
    let args = match lire_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}\nlauncher: {}", USAGE, e);
            std::process::exit(2);
        }
    };
    let racine = crate::racine().to_path_buf();
    if args.compte {
        let ok = compte::creer_compte(&racine.join("compte.local.json"), &racine.join("sessions.local.json"), &mut lire_ligne, &mut |t: &str| afficher(t));
        std::process::exit(if ok { 0 } else { 1 });
    }
    let chemins = Chemins::de(&racine);
    let mut conf = match config::charger(&chemins.catalogue, &chemins.local) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Configuration illisible : {}", e);
            std::process::exit(2);
        }
    };
    if let Some(d) = &args.dossier {
        conf.insert("dossier_outils".into(), json!(config::chemin_absolu(d).to_string_lossy()));
    }
    if let Some(p) = args.port {
        conf.insert("port".into(), json!(p));
        conf.insert("port_secours".into(), Value::Null);
    }
    let entier = |k: &str| conf.get(k).and_then(Value::as_u64).map(|p| p as u16);
    let dossier_outils = conf.get("dossier_outils").and_then(Value::as_str).unwrap_or("").to_string();

    afficher("Web Launcher");
    afficher(&format!("  Outils dans {}", dossier_outils));
    let srv = ouvrir(entier("port").unwrap_or(80), entier("port_secours"));
    let port = srv.port();
    let schema = match appairage::contexte_tls() {
        Ok(cfg) => {
            srv.brancher_tls(cfg);
            "https"
        }
        Err(raison) => {
            afficher(&format!("  HTTP en clair sur le réseau ({}) : le mot de passe peut être lu sur le Wi-Fi.", raison));
            "http"
        }
    };

    let andy = Andy::new(&racine);
    andy.historique(); // reprend l'historique d'Andy de l'ancien emplacement, s'il le faut
    let exe = std::env::current_exe().unwrap_or_else(|_| racine.join("bin/launcher"));
    let nom_mdns = conf.get("nom_mdns").and_then(Value::as_str).map(str::to_string);
    let port_relais = entier("port_relais").unwrap_or(18089);
    let port_public = conf.get("port_public_relais").cloned().unwrap_or(json!(8443));
    let g = Gestionnaire::new(conf, chemins.clone(), exe.clone(), Box::new(afficher));
    g.reprendre();

    let alias = AliasMdns::new(if args.sans_mdns { None } else { nom_mdns.clone() });
    alias.demarrer(afficher);
    let gardien = Arc::new(Gardien::new(&racine.join("compte.local.json"), &racine.join("sessions.local.json")));
    if !gardien.actif() {
        afficher(
            "  [!] Aucun mot de passe : seul ce poste (127.0.0.1) a accès, les autres appareils sont refusés. \
             Créer le compte : sh lancer.sh --compte",
        );
    }
    let mut infos = Map::new();
    infos.insert("port".into(), json!(port));
    infos.insert("nom_mdns".into(), json!(nom_mdns));
    infos.insert("port_public_relais".into(), port_public);
    infos.insert("nom_machine".into(), json!(reseau::nom_machine()));
    infos.insert("dossier_outils".into(), json!(dossier_outils));
    infos.insert("demarre".into(), json!(maintenant() as i64));
    let alias_relance = alias.clone();
    let app = Arc::new(App {
        g: g.clone(),
        gardien,
        andy,
        claude: Claude::new(&racine),
        alias: Some(alias.clone()),
        infos,
        visites: Visites::new("launcher"),
        web: racine.join("web"),
        relancer: Arc::new(move || {
            alias_relance.arreter();
            relancer_processus(&exe);
        }),
    });
    let surveillant = g.clone();
    std::thread::spawn(move || surveillant.surveiller());
    // les outils depuis Internet : adresse à part (Caddy -> 127.0.0.1:port_relais), voir README
    crate::relais::ouvrir(port_relais, app.clone());

    afficher("  Ouvrir dans un navigateur :");
    if let Some(ip) = reseau::ip_locale() {
        afficher(&format!("    {}://{}:{}/", schema, ip, port));
    }
    if let Some(ts) = reseau::ip_tailscale() {
        afficher(&format!("    {}://{}:{}/   (Tailscale)", schema, ts, port));
    }
    if let (Some(nom), false) = (&nom_mdns, args.sans_mdns) {
        afficher(&format!("    {}://{}.local:{}/", schema, nom, port));
    }
    afficher(&format!("    {}://{}.local:{}/", schema, reseau::nom_machine(), port));
    srv.servir(Arc::new(move |r: &mut Requete| servir(r, &app)));
}

#[cfg(test)]
mod essais {
    use super::*;

    fn a(l: &[&str]) -> Vec<String> {
        l.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn ligne_de_commande() {
        assert_eq!(lire_args(&a(&["--port", "8090", "--sans-mdns"])).unwrap(), Args { port: Some(8090), sans_mdns: true, ..Default::default() });
        assert_eq!(lire_args(&a(&["--port=80", "--dossier=~/x"])).unwrap().dossier.as_deref(), Some("~/x"));
        assert!(lire_args(&a(&["--compte"])).unwrap().compte);
        assert!(lire_args(&a(&["--port"])).is_err());
        assert!(lire_args(&a(&["--port", "abc"])).is_err());
        assert!(lire_args(&a(&["--inconnu"])).is_err());
    }

    #[test]
    fn chemins_normalises() {
        for c in ["/", "/css/style.css", "/css/", "/api/etat"] {
            assert!(chemin_normal(c), "{c}");
        }
        for c in ["/css/../index.html", "/css//x", "/a/./b", "///", "x"] {
            assert!(!chemin_normal(c), "{c}");
        }
    }

    #[test]
    fn saisie_et_jeton() {
        assert_eq!(filtrer_saisie("o\r\x1a\x1b[A\x03"), "o\r[A\x03");
        assert_eq!(masquer("GET /?jeton=abc-123 HTTP"), "GET /?jeton=••• HTTP");
        assert_eq!(quote("bin é.dat"), "bin%20%C3%A9.dat");
    }

    #[test]
    fn lecture_du_journal() {
        let d = std::env::temp_dir().join(format!("lanceur-journal-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let j = d.join("x.log");
        std::fs::write(&j, "un\r\ndeux\nété".as_bytes()).unwrap();
        assert_eq!(lire_fin(&j, 2), "deux\nété");
        // UTF-8 coupé : la fin attend le lot suivant
        let mut o = b"ab\xc3".to_vec();
        std::fs::write(&j, &o).unwrap();
        assert_eq!(lire_depuis(&j, 0), (2, "ab".to_string()));
        o.push(0xa9);
        std::fs::write(&j, &o).unwrap();
        assert_eq!(lire_depuis(&j, 2), (4, "é".to_string()));
        // un jeton pas fini n'est jamais montré
        std::fs::write(&j, "ligne\nhttp://x/?jeton=ab").unwrap();
        assert_eq!(lire_depuis(&j, 0), (6, "ligne\n".to_string()));
        std::fs::write(&j, "http://x/?jeton=abc\n").unwrap();
        assert_eq!(lire_depuis(&j, 0).1, "http://x/?jeton=•••\n");
        assert_eq!(lire_fin(&d.join("absent"), 5), "(journal vide)");
        let _ = std::fs::remove_dir_all(&d);
    }
}
