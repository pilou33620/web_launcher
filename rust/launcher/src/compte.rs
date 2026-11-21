//! Comptes du launcher : un ou plusieurs (mêmes droits), mots de passe jamais stockés en clair.
//!
//! - Mot de passe : haché avec scrypt (lent et gourmand en mémoire exprès, pour rendre inutile le
//!   vol du fichier), sel aléatoire de 16 octets. Les empreintes PBKDF2-SHA256 de l'ancienne
//!   version (Python sans scrypt) restent reconnues. Fichier compte.local.json, en 0600.
//! - Sessions : jeton aléatoire de 256 bits dans un cookie HttpOnly, SameSite=Strict. Le serveur
//!   n'en garde que l'empreinte SHA-256 (sessions.local.json) : le fichier ne permet pas de se
//!   connecter. Valables 30 jours, gardées aux redémarrages du launcher.
//! - Essais (force brute) : 5 erreurs depuis une même adresse la bloquent 5 min, puis 10, 20...
//!   jusqu'à 24 h en cas de récidive ; 10 erreurs sur un même identifiant (toutes adresses) le
//!   bloquent 15 min à 1 h ; plus de 30 erreurs en 15 min, toutes adresses confondues, bloquent
//!   toute connexion 15 min. Chaque erreur coûte en plus 1 s. Une connexion réussie efface les
//!   erreurs de son adresse et de son identifiant. Une adresse qui s'est déjà connectée avec succès
//!   échappe aux blocages « identifiant » et « total » ; seul son propre compteur s'applique.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::{alea, ecrire_json, maintenant, tenir};

pub const DUREE_SESSION: u64 = 30 * 24 * 3600;
pub const ESSAIS: u32 = 5; // par adresse
const BLOCAGE: f64 = 300.0;
const BLOCAGE_MAX: f64 = 24.0 * 3600.0;
pub const ESSAIS_COMPTE: u32 = 10; // par identifiant
const BLOCAGE_COMPTE: f64 = 900.0;
const BLOCAGE_COMPTE_MAX: f64 = 3600.0;
pub const ESSAIS_TOTAL: usize = 30; // toutes adresses
const FENETRE_TOTAL: f64 = 900.0;
const BLOCAGE_TOTAL: f64 = 900.0;
const OUBLI: f64 = 24.0 * 3600.0; // sans erreur pendant 24 h : compteurs remis à zéro
pub const LONGUEUR_MIN: usize = 12;
const PBKDF2_TOURS: u64 = 600_000;

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// L'empreinte d'un mot de passe : {"algo", "params", "sel", "empreinte"} (format de la version Python).
pub fn hacher(mdp: &str, sel: Option<&[u8]>, algo: &str, params: Option<&Value>) -> Result<Map<String, Value>, String> {
    let sel = sel.map(<[u8]>::to_vec).unwrap_or_else(|| alea(16));
    let mut sortie = [0u8; 32];
    let params = if algo == "scrypt" {
        let p = params.cloned().unwrap_or_else(|| json!({"n": 32768, "r": 8, "p": 1}));
        let (n, r, pp) = (p["n"].as_u64().unwrap_or(0), p["r"].as_u64().unwrap_or(0), p["p"].as_u64().unwrap_or(0));
        if !n.is_power_of_two() || n < 2 {
            return Err("paramètres scrypt invalides".into());
        }
        let reglage = scrypt::Params::new(n.trailing_zeros() as u8, r as u32, pp as u32, 32).map_err(|e| e.to_string())?;
        scrypt::scrypt(mdp.as_bytes(), &sel, &reglage, &mut sortie).map_err(|e| e.to_string())?;
        p
    } else if algo == "pbkdf2_sha256" {
        let p = params.cloned().unwrap_or_else(|| json!({"tours": PBKDF2_TOURS}));
        let tours = p["tours"].as_u64().unwrap_or(0).clamp(1, 10_000_000) as u32;
        pbkdf2::pbkdf2_hmac::<Sha256>(mdp.as_bytes(), &sel, tours, &mut sortie);
        p
    } else {
        return Err(format!("algorithme inconnu : {}", algo));
    };
    let mut m = Map::new();
    m.insert("algo".into(), json!(algo));
    m.insert("params".into(), params);
    m.insert("sel".into(), json!(b64().encode(&sel)));
    m.insert("empreinte".into(), json!(b64().encode(sortie)));
    Ok(m)
}

/// Les comptes. L'ancien fichier à un seul compte est lu comme une liste d'un.
pub fn lire_comptes(chemin: &Path) -> Vec<Map<String, Value>> {
    let Ok(octets) = std::fs::read(chemin) else { return Vec::new() };
    let Ok(Value::Object(donnees)) = serde_json::from_slice::<Value>(&octets) else { return Vec::new() };
    let comptes = match donnees.get("comptes") {
        Some(Value::Array(a)) => a.clone(),
        Some(_) => Vec::new(),
        None => vec![Value::Object(donnees.clone())],
    };
    comptes
        .into_iter()
        .filter_map(|c| match c {
            Value::Object(m) if m.get("login").is_some_and(|l| l.is_string() && crate::vrai(l)) && m.get("empreinte").is_some_and(crate::vrai) => Some(m),
            _ => None,
        })
        .collect()
}

fn login_de(c: &Map<String, Value>) -> &str {
    c.get("login").and_then(Value::as_str).unwrap_or("")
}

fn trouver(comptes: &[Map<String, Value>], login: &str) -> Option<usize> {
    let login = login.trim().to_lowercase();
    comptes.iter().position(|c| login_de(c).to_lowercase() == login)
}

/// Message d'erreur, ou None si l'identifiant convient.
pub fn verifier_login(login: &str) -> Option<String> {
    if login.is_empty() {
        return Some("Identifiant vide.".into());
    }
    let imprimable = login.chars().all(|c| !c.is_control() && (c == ' ' || !c.is_whitespace()));
    if login.chars().count() > 40 || login.contains([' ', '"', '\\']) || !imprimable {
        return Some("Identifiant : 40 caractères au plus, sans espace ni guillemet.".into());
    }
    None
}

pub fn verifier_mdp(mdp: &str) -> Option<String> {
    (mdp.chars().count() < LONGUEUR_MIN).then(|| format!("Mot de passe trop court ({} caractères au moins).", LONGUEUR_MIN))
}

#[derive(Clone, Debug)]
pub struct Echec {
    pub n: u32,
    pub blocages: u32,
    pub jusqu: f64,
    pub vu: f64,
}

fn attente(e: Option<&Echec>, maintenant: f64) -> f64 {
    e.map(|e| e.jusqu - maintenant).unwrap_or(0.0)
}

/// Une erreur de plus pour cette clé. Au seuil : blocage, doublé à chaque récidive.
fn compter(table: &mut HashMap<String, Echec>, cle: &str, seuil: u32, base: f64, plafond: f64, maintenant: f64) -> Echec {
    let mut e = match table.get(cle) {
        Some(e) if maintenant - e.vu <= OUBLI => e.clone(),
        _ => Echec { n: 0, blocages: 0, jusqu: 0.0, vu: maintenant },
    };
    e.n += 1;
    e.vu = maintenant;
    if e.n >= seuil {
        e.jusqu = maintenant + (base * 2f64.powi(e.blocages.min(60) as i32)).min(plafond);
        e.blocages += 1;
        e.n = 0;
    }
    table.insert(cle.to_string(), e.clone());
    e
}

fn duree(secondes: f64) -> String {
    let minutes = (secondes / 60.0).floor() as i64 + 1;
    if minutes < 120 {
        format!("{} min", minutes)
    } else {
        format!("{} h", minutes / 60 + i64::from(minutes % 60 > 0))
    }
}

/// Une session ouverte : sa fin, et le compte (None : ancien format sans compte connu).
#[derive(Clone, Debug)]
pub struct Session {
    pub fin: f64,
    pub login: Option<String>,
}

#[derive(Default)]
pub struct Interne {
    pub echecs: HashMap<String, Echec>,         // ip -> compteur
    pub echecs_comptes: HashMap<String, Echec>, // identifiant (minuscules) -> compteur
    pub echecs_total: Vec<f64>,                 // heures des dernières erreurs, toutes adresses
    pub total_jusqu: f64,
    pub connues: HashMap<String, f64>, // ip -> dernière connexion réussie
    sessions: HashMap<String, Session>,
    date_fichier: Option<SystemTime>,
}

/// Vérifie les identifiants, tient les comptes et les sessions ouvertes.
pub struct Gardien {
    pub fichier: PathBuf,
    pub fichier_sessions: PathBuf,
    pub interne: Mutex<Interne>,
    calculs: (Mutex<u32>, Condvar), // scrypt prend 32 Mo : pas plus de 2 à la fois
    pub pause_echec: Duration,      // chaque erreur coûte 1 s de plus
    pub suivis_max: usize,          // adresses / identifiants suivis au plus (mémoire bornée)
}

impl Gardien {
    pub fn new(fichier: &Path, sessions: &Path) -> Gardien {
        let g = Gardien {
            fichier: fichier.to_path_buf(),
            fichier_sessions: sessions.to_path_buf(),
            interne: Mutex::new(Interne::default()),
            calculs: (Mutex::new(0), Condvar::new()),
            pause_echec: Duration::from_secs(1),
            suivis_max: 5000,
        };
        let mut i = tenir(&g.interne);
        g.lire_sessions(&mut i);
        drop(i);
        g
    }

    pub fn actif(&self) -> bool {
        !lire_comptes(&self.fichier).is_empty()
    }

    fn hacher_borne(&self, mdp: &str, sel: Option<&[u8]>, algo: &str, params: Option<&Value>) -> Result<Map<String, Value>, String> {
        let (places, signal) = &self.calculs;
        let mut n = tenir(places);
        while *n >= 2 {
            n = signal.wait(n).unwrap_or_else(|e| e.into_inner());
        }
        *n += 1;
        drop(n);
        let r = hacher(mdp, sel, algo, params);
        *tenir(places) -= 1;
        signal.notify_one();
        r
    }

    /// Ok(jeton de session) ou Err(message d'erreur).
    pub fn verifier(&self, login: &str, mdp: &str, ip: &str) -> Result<String, String> {
        let comptes = lire_comptes(&self.fichier);
        if comptes.is_empty() {
            return Err("Aucun compte : le créer sur le Raspberry Pi (launcher --compte).".into());
        }
        // l'essai est compté avant le calcul : des requêtes simultanées ne passent pas toutes
        let cle: String = login.trim().to_lowercase().chars().take(40).collect();
        let e_ip = {
            let mut i = tenir(&self.interne);
            let t = maintenant();
            self.purger(&mut i, t);
            let connue = ip != "internet" && t - i.connues.get(ip).copied().unwrap_or(-OUBLI * 30.0) < OUBLI * 30.0;
            let mut a = attente(i.echecs.get(ip), t);
            if !connue {
                a = a.max(attente(i.echecs_comptes.get(&cle), t)).max(i.total_jusqu - t);
            }
            if a > 0.0 {
                return Err(format!("Trop d'essais : réessayez dans {}.", duree(a)));
            }
            let e = compter(&mut i.echecs, ip, ESSAIS, BLOCAGE, BLOCAGE_MAX, t);
            compter(&mut i.echecs_comptes, &cle, ESSAIS_COMPTE, BLOCAGE_COMPTE, BLOCAGE_COMPTE_MAX, t);
            i.echecs_total.push(t);
            if i.echecs_total.len() >= ESSAIS_TOTAL {
                i.total_jusqu = t + BLOCAGE_TOTAL;
                i.echecs_total.clear();
            }
            e
        };
        // identifiant inconnu : le calcul est fait quand même (même durée, rien n'est révélé)
        let compte = trouver(&comptes, login);
        let modele = &comptes[compte.unwrap_or(0)];
        let champ = |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let sel = b64().decode(champ(modele, "sel")).unwrap_or_default();
        let essai = self.hacher_borne(mdp, Some(&sel), &champ(modele, "algo"), modele.get("params"));
        let bon = match (compte, essai) {
            (Some(c), Ok(e)) => {
                let attendu = champ(&comptes[c], "empreinte");
                !attendu.is_empty() && web_tools_commun::appairage::egal(&champ(&e, "empreinte"), &attendu)
            }
            _ => false,
        };
        if !bon {
            std::thread::sleep(self.pause_echec);
            let t = maintenant();
            if e_ip.jusqu > t {
                return Err(format!("Identifiant ou mot de passe incorrect : adresse bloquée {}.", duree(e_ip.jusqu - t)));
            }
            return Err(format!("Identifiant ou mot de passe incorrect ({} essai(s) restant(s)).", ESSAIS - e_ip.n));
        }
        let mut i = tenir(&self.interne);
        i.echecs.remove(ip);
        i.echecs_comptes.remove(&cle);
        if ip != "internet" {
            i.connues.insert(ip.to_string(), maintenant());
            if i.connues.len() > self.suivis_max {
                if let Some(vieille) = i.connues.iter().min_by(|a, b| a.1.total_cmp(b.1)).map(|(k, _)| k.clone()) {
                    i.connues.remove(&vieille);
                }
            }
        }
        i.echecs_total.pop();
        let jeton = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(alea(32));
        self.relire(&mut i);
        let login = login_de(&comptes[compte.unwrap_or(0)]).to_string();
        i.sessions.insert(empreinte(&jeton), Session { fin: maintenant() + DUREE_SESSION as f64, login: Some(login) });
        self.sauver_sessions(&mut i);
        Ok(jeton)
    }

    pub fn purger(&self, i: &mut Interne, t: f64) {
        i.echecs_total.retain(|x| t - x < FENETRE_TOTAL);
        for table in [&mut i.echecs, &mut i.echecs_comptes] {
            table.retain(|_, e| !(e.jusqu < t && t - e.vu > OUBLI));
            if table.len() > self.suivis_max {
                // attaque depuis de très nombreuses adresses : les plus anciennes sont oubliées
                let mut cles: Vec<(String, f64)> = table.iter().map(|(k, e)| (k.clone(), e.vu)).collect();
                cles.sort_by(|a, b| a.1.total_cmp(&b.1));
                for (k, _) in cles.iter().take(table.len() - self.suivis_max) {
                    table.remove(k);
                }
            }
        }
    }

    // -- comptes (depuis la page) ----------------------------------------

    pub fn comptes(&self) -> Vec<Value> {
        lire_comptes(&self.fichier).iter().map(|c| json!({"login": c.get("login"), "cree_le": c.get("cree_le")})).collect()
    }

    fn ecrire(&self, comptes: Vec<Map<String, Value>>) -> Result<(), String> {
        let liste: Vec<Value> = comptes.into_iter().map(Value::Object).collect();
        ecrire_json(&self.fichier, &json!({ "comptes": liste }), 1, true, false).map_err(|e| e.to_string())
    }

    /// Nouveau compte. Rend un message d'erreur, ou None.
    pub fn ajouter(&self, login: &str, mdp: &str) -> Option<String> {
        let login = login.trim();
        if let Some(e) = verifier_login(login).or_else(|| verifier_mdp(mdp)) {
            return Some(e);
        }
        let mut nouveau = match self.hacher_borne(mdp, None, "scrypt", None) {
            Ok(n) => n,
            Err(e) => return Some(e),
        };
        nouveau.insert("login".into(), json!(login));
        nouveau.insert("cree_le".into(), json!(crate::heure("%Y-%m-%d %H:%M")));
        let _i = tenir(&self.interne);
        let mut comptes = lire_comptes(&self.fichier);
        if trouver(&comptes, login).is_some() {
            return Some(format!("Le compte « {} » existe déjà.", login));
        }
        comptes.push(nouveau);
        self.ecrire(comptes).err()
    }

    /// Nouveau mot de passe ; les appareils connectés à ce compte sont déconnectés, sauf la
    /// session « garder » (celle qui fait le changement).
    pub fn changer_mdp(&self, login: &str, mdp: &str, garder: Option<&str>) -> Option<String> {
        if let Some(e) = verifier_mdp(mdp) {
            return Some(e);
        }
        let empreinte_neuve = match self.hacher_borne(mdp, None, "scrypt", None) {
            Ok(n) => n,
            Err(e) => return Some(e),
        };
        let mut i = tenir(&self.interne);
        let mut comptes = lire_comptes(&self.fichier);
        let Some(c) = trouver(&comptes, login) else { return Some("Compte inconnu.".into()) };
        for (k, v) in empreinte_neuve {
            comptes[c].insert(k, v);
        }
        let nom = login_de(&comptes[c]).to_string();
        if let Err(e) = self.ecrire(comptes) {
            return Some(e);
        }
        self.fermer_compte(&mut i, &nom, garder);
        None
    }

    pub fn supprimer(&self, login: &str) -> Option<String> {
        let mut i = tenir(&self.interne);
        let mut comptes = lire_comptes(&self.fichier);
        let Some(c) = trouver(&comptes, login) else { return Some("Compte inconnu.".into()) };
        if comptes.len() == 1 {
            return Some("C'est le dernier compte : il ne peut pas être supprimé.".into());
        }
        let nom = login_de(&comptes.remove(c)).to_string();
        if let Err(e) = self.ecrire(comptes) {
            return Some(e);
        }
        self.fermer_compte(&mut i, &nom, None);
        None
    }

    fn fermer_compte(&self, i: &mut Interne, login: &str, garder: Option<&str>) {
        self.relire(i);
        let garder = garder.map(empreinte);
        let login = login.to_lowercase();
        i.sessions.retain(|k, v| Some(k) == garder.as_ref() || v.login.as_deref().unwrap_or("").to_lowercase() != login);
        self.sauver_sessions(i);
    }

    // -- sessions --------------------------------------------------------

    /// Identifiant du compte de cette session, ou None (session absente, expirée, compte supprimé).
    pub fn utilisateur(&self, jeton: Option<&str>) -> Option<String> {
        let jeton = jeton.filter(|j| !j.is_empty())?;
        let s = {
            let mut i = tenir(&self.interne);
            self.relire(&mut i);
            let s = i.sessions.get(&empreinte(jeton)).cloned();
            s
        }?;
        if s.fin <= maintenant() {
            return None;
        }
        let comptes = lire_comptes(&self.fichier);
        trouver(&comptes, s.login.as_deref().unwrap_or("")).map(|c| login_de(&comptes[c]).to_string())
    }

    pub fn fermer(&self, jeton: Option<&str>) {
        let mut i = tenir(&self.interne);
        self.relire(&mut i);
        if i.sessions.remove(&empreinte(jeton.unwrap_or(""))).is_some() {
            self.sauver_sessions(&mut i);
        }
    }

    fn date(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.fichier_sessions).and_then(|m| m.modified()).ok()
    }

    /// Changé par « launcher --compte » : relu.
    fn relire(&self, i: &mut Interne) {
        if self.date() != i.date_fichier {
            self.lire_sessions(i);
        }
    }

    fn lire_sessions(&self, i: &mut Interne) {
        i.date_fichier = self.date();
        i.sessions.clear();
        let Ok(octets) = std::fs::read(&self.fichier_sessions) else { return };
        let Ok(Value::Object(sessions)) = serde_json::from_slice::<Value>(&octets) else { return };
        let t = maintenant();
        let comptes = lire_comptes(&self.fichier);
        let ancien = (comptes.len() == 1).then(|| login_de(&comptes[0]).to_string());
        for (k, v) in sessions {
            let s = match &v {
                Value::Number(n) => Session { fin: n.as_f64().unwrap_or(0.0), login: ancien.clone() }, // ancien format
                Value::Object(m) => match m.get("fin").and_then(Value::as_f64) {
                    Some(fin) => Session { fin, login: m.get("login").and_then(Value::as_str).map(str::to_string) },
                    None => continue,
                },
                _ => continue,
            };
            if s.fin > t {
                i.sessions.insert(k, s);
            }
        }
    }

    fn sauver_sessions(&self, i: &mut Interne) {
        let t = maintenant();
        i.sessions.retain(|_, s| s.fin > t);
        let mut m = Map::new();
        for (k, s) in &i.sessions {
            m.insert(k.clone(), json!({"fin": s.fin, "login": s.login}));
        }
        if ecrire_json(&self.fichier_sessions, &Value::Object(m), 1, true, false).is_ok() {
            i.date_fichier = self.date();
        }
    }
}

pub fn empreinte(jeton: &str) -> String {
    Sha256::digest(jeton.as_bytes()).iter().map(|b| format!("{:02x}", b)).collect()
}

/// En console : ajoute un compte, ou change le mot de passe d'un compte existant (ses appareils
/// connectés sont alors déconnectés). Les autres comptes ne bougent pas.
/// `lire(invite, secret)` rend la ligne saisie ; `secret` : sans écho.
pub fn creer_compte(fichier: &Path, sessions: &Path, lire: &mut dyn FnMut(&str, bool) -> String, ecrire: &mut dyn FnMut(&str)) -> bool {
    let comptes = lire_comptes(fichier);
    let noms: Vec<&str> = comptes.iter().map(login_de).collect();
    ecrire(&format!(
        "Comptes du Web Launcher{}",
        if noms.is_empty() { " : aucun pour l'instant".to_string() } else { format!(" (existants : {})", noms.join(", ")) }
    ));
    let seul = (comptes.len() == 1).then(|| login_de(&comptes[0]).to_string());
    let invite = format!("  Identifiant (nouveau ou existant){} : ", seul.as_ref().map(|s| format!(" [{}]", s)).unwrap_or_default());
    let mut login = lire(&invite, false).trim().to_string();
    if login.is_empty() {
        login = seul.unwrap_or_default();
    }
    if let Some(e) = verifier_login(&login) {
        ecrire(&format!("  {} Rien n'est changé.", e));
        return false;
    }
    let existant = trouver(&comptes, &login).map(|c| login_de(&comptes[c]).to_string());
    ecrire(&match &existant {
        Some(nom) => format!("  Nouveau mot de passe de « {} ».", nom),
        None => format!("  Nouveau compte « {} ».", login),
    });
    let mdp = loop {
        let mdp = lire(&format!("  Mot de passe ({} caractères au moins) : ", LONGUEUR_MIN), true);
        if verifier_mdp(&mdp).is_some() {
            ecrire("  Trop court.");
            continue;
        }
        if lire("  Encore une fois : ", true) != mdp {
            ecrire("  Les deux saisies diffèrent.");
            continue;
        }
        break mdp;
    };
    let g = Gardien::new(fichier, sessions);
    let erreur = match &existant {
        Some(nom) => g.changer_mdp(nom, &mdp, None),
        None => g.ajouter(&login, &mdp),
    };
    if let Some(e) = erreur {
        ecrire(&format!("  {}", e));
        return false;
    }
    ecrire(&format!(
        "  Compte enregistré (mot de passe haché).{}",
        if existant.is_some() { " Les appareils connectés à ce compte sont déconnectés." } else { "" }
    ));
    true
}

#[cfg(test)]
mod essais {
    use super::*;

    /// hashlib.scrypt(b"correct horse", salt=b"0123456789abcdef", n=2**15, r=8, p=1, dklen=32), en base64
    const SCRYPT_PYTHON: &str = "puHF8wsl4P5tR3JANsCleZiOQes+05KY2PoHbUEpMGQ=";

    fn gardien(nom: &str) -> (Gardien, PathBuf) {
        let d = std::env::temp_dir().join(format!("lanceur-compte-{}-{}", nom, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (f, s) = (d.join("compte.json"), d.join("sessions.json"));
        let mut saisies = ["pilou", "court", "correct horse", "autre chose", "correct horse", "correct horse"].into_iter();
        assert!(creer_compte(&f, &s, &mut |_, _| saisies.next().unwrap().to_string(), &mut |_| {}));
        let mut g = Gardien::new(&f, &s);
        g.pause_echec = Duration::ZERO;
        (g, d)
    }

    fn valide(g: &Gardien, jeton: &str) -> bool {
        g.utilisateur(Some(jeton)).is_some()
    }

    #[test]
    fn empreinte_identique_a_python() {
        let h = hacher("correct horse", Some(b"0123456789abcdef"), "scrypt", None).unwrap();
        assert_eq!(h["params"], json!({"n": 32768, "r": 8, "p": 1}));
        assert_eq!(h["sel"], "MDEyMzQ1Njc4OWFiY2RlZg==");
        assert_eq!(h["empreinte"], SCRYPT_PYTHON);
        // hashlib.pbkdf2_hmac("sha256", b"correct horse", b"0123456789abcdef", 1000)
        let p = hacher("correct horse", Some(b"0123456789abcdef"), "pbkdf2_sha256", Some(&json!({"tours": 1000}))).unwrap();
        assert_eq!(p["empreinte"], "cBg8D2DungRB9k76szThf5ehfyBz991ay6PT8Srwk4M=");
    }

    #[test]
    fn mot_de_passe_jamais_en_clair() {
        let (g, d) = gardien("clair");
        let contenu = std::fs::read_to_string(&g.fichier).unwrap();
        assert!(!contenu.contains("correct horse"));
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&g.fichier).unwrap().permissions()) & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn connexion_sessions_et_blocage() {
        let (g, d) = gardien("sessions");
        let jeton = g.verifier("Pilou", "correct horse", "10.0.0.1").unwrap();
        assert!(valide(&g, &jeton));
        assert!(!std::fs::read_to_string(&g.fichier_sessions).unwrap().contains(&jeton)); // seule l'empreinte est gardée
        assert!(valide(&Gardien::new(&g.fichier, &g.fichier_sessions), &jeton));
        g.fermer(Some(&jeton));
        assert!(!valide(&g, &jeton));
        for _ in 0..ESSAIS {
            assert!(g.verifier("pilou", "faux", "10.0.0.2").is_err());
        }
        // adresse bloquée, même avec le bon mot de passe ; les autres adresses non
        assert!(g.verifier("pilou", "correct horse", "10.0.0.2").is_err());
        assert!(g.verifier("pilou", "correct horse", "10.0.0.3").is_ok());
        assert!(g.verifier("autre", "correct horse", "10.0.0.4").is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn plusieurs_comptes() {
        let (g, d) = gardien("plusieurs");
        assert!(g.ajouter("PILOU", "un autre mdp").unwrap().contains("existe"));
        assert!(g.ajouter("marie", "court").unwrap().contains("court"));
        assert_eq!(g.ajouter("marie", "mot de passe marie"), None);
        let noms: Vec<Value> = g.comptes().iter().map(|c| c["login"].clone()).collect();
        assert_eq!(noms, vec![json!("pilou"), json!("marie")]);
        let j_pilou = g.verifier("pilou", "correct horse", "10.0.0.1").unwrap();
        let j_marie = g.verifier("Marie", "mot de passe marie", "10.0.0.1").unwrap();
        let j_marie2 = g.verifier("marie", "mot de passe marie", "10.0.0.5").unwrap();
        assert_eq!(g.utilisateur(Some(&j_marie)).as_deref(), Some("marie"));
        assert!(g.verifier("marie", "correct horse", "10.0.0.1").is_err());
        // nouveau mot de passe : les autres appareils de marie déconnectés, pas celui qui le change
        assert_eq!(g.changer_mdp("marie", "nouveau mdp marie", Some(&j_marie)), None);
        assert!(valide(&g, &j_marie));
        assert!(!valide(&g, &j_marie2));
        assert!(valide(&g, &j_pilou));
        assert!(g.verifier("marie", "nouveau mdp marie", "10.0.0.1").is_ok());
        // suppression : ses sessions tombent ; le dernier compte reste
        assert_eq!(g.supprimer("marie"), None);
        assert!(!valide(&g, &j_marie));
        assert!(valide(&g, &j_pilou));
        assert!(g.supprimer("pilou").unwrap().contains("dernier"));
        // en console : un identifiant existant change son mot de passe, un nouveau est ajouté
        let mut saisies = ["paul", "mdp de paul!", "mdp de paul!"].into_iter();
        assert!(creer_compte(&g.fichier, &g.fichier_sessions, &mut |_, _| saisies.next().unwrap().to_string(), &mut |_| {}));
        let noms: Vec<Value> = g.comptes().iter().map(|c| c["login"].clone()).collect();
        assert_eq!(noms, vec![json!("pilou"), json!("paul")]);
        assert!(valide(&g, &j_pilou));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn ancien_fichier_a_un_compte() {
        let (g, d) = gardien("ancien");
        let jeton = g.verifier("pilou", "correct horse", "10.0.0.1").unwrap();
        let ancien = lire_comptes(&g.fichier).remove(0);
        std::fs::write(&g.fichier, Value::Object(ancien).to_string()).unwrap(); // format d'avant : un seul compte
        std::fs::write(&g.fichier_sessions, json!({ empreinte(&jeton): maintenant() + 60.0 }).to_string()).unwrap();
        let g2 = Gardien::new(&g.fichier, &g.fichier_sessions);
        assert_eq!(g2.utilisateur(Some(&jeton)).as_deref(), Some("pilou"));
        assert_eq!(g2.ajouter("marie", "mot de passe marie"), None);
        assert_eq!(g2.comptes().len(), 2);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn vrai_fichier_de_la_version_python() {
        // compte.local.json écrit par la version Python (mot de passe « correct horse »)
        let d = std::env::temp_dir().join(format!("lanceur-compte-py-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("compte.json");
        let texte = format!(
            "{{\n \"comptes\": [\n  {{\n   \"algo\": \"scrypt\",\n   \"params\": {{\n    \"n\": 32768,\n    \"r\": 8,\n    \"p\": 1\n   }},\n   \"sel\": \"MDEyMzQ1Njc4OWFiY2RlZg==\",\n   \"empreinte\": \"{}\",\n   \"login\": \"pilou\",\n   \"cree_le\": \"2026-10-12 18:36\"\n  }}\n ]\n}}",
            SCRYPT_PYTHON
        );
        std::fs::write(&f, texte).unwrap();
        let mut g = Gardien::new(&f, &d.join("s.json"));
        g.pause_echec = Duration::ZERO;
        assert!(g.verifier("pilou", "correct horse", "10.0.0.1").is_ok());
        assert_eq!(g.comptes(), vec![json!({"login": "pilou", "cree_le": "2026-10-12 18:36"})]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn force_brute_adresse_recidive() {
        let (g, d) = gardien("recidive");
        let essayer = |n: u32| {
            for _ in 0..n {
                let _ = g.verifier("pilou", "faux", "10.0.0.2");
            }
            g.verifier("pilou", "correct horse", "10.0.0.2")
        };
        let e = essayer(ESSAIS).unwrap_err();
        assert!(e.starts_with("Trop d'essais : réessayez dans 5 min"), "{e}");
        let lever = |blocages: Option<u32>| {
            let mut i = tenir(&g.interne);
            let e = i.echecs.get_mut("10.0.0.2").unwrap();
            e.jusqu = 0.0; // le blocage est passé
            if let Some(b) = blocages {
                e.blocages = b;
            }
            i.echecs_comptes.clear(); // (testé à part : blocage de l'identifiant)
        };
        lever(None);
        assert!(essayer(ESSAIS).unwrap_err().contains("10 min")); // récidive : doublé
        lever(Some(20));
        assert!(essayer(ESSAIS).unwrap_err().contains("24 h")); // plafond
        lever(None);
        assert!(g.verifier("pilou", "correct horse", "10.0.0.2").is_ok());
        assert!(!tenir(&g.interne).echecs.contains_key("10.0.0.2")); // réussite : effacé
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn force_brute_identifiant_et_total() {
        let (g, d) = gardien("total");
        // 10 erreurs sur « pilou » depuis 10 adresses différentes : l'identifiant est bloqué
        for i in 0..ESSAIS_COMPTE {
            let _ = g.verifier("Pilou ", "faux", &format!("10.1.0.{}", i));
        }
        assert!(g.verifier("pilou", "correct horse", "10.2.0.1").unwrap_err().starts_with("Trop d'essais"));
        assert!(!g.verifier("marie", "x", "10.2.0.2").unwrap_err().starts_with("Trop")); // un autre identifiant : essayé
        // une adresse déjà connectée avec succès passe malgré le blocage de l'identifiant
        tenir(&g.interne).connues.insert("10.9.9.9".into(), maintenant());
        assert!(g.verifier("pilou", "correct horse", "10.9.9.9").is_ok());
        // attaque répartie sur beaucoup d'adresses et d'identifiants : tout est bloqué
        tenir(&g.interne).echecs_comptes.clear();
        for i in 0..ESSAIS_TOTAL {
            let _ = g.verifier(&format!("nom{}", i), "faux", &format!("10.3.{}.1", i));
        }
        assert!(g.verifier("pilou", "correct horse", "10.4.0.1").unwrap_err().starts_with("Trop d'essais"));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn force_brute_memoire_bornee() {
        let (mut g, d) = gardien("borne");
        g.suivis_max = 3;
        for n in 0..6 {
            {
                let mut i = tenir(&g.interne);
                i.echecs_total.clear();
                i.total_jusqu = 0.0;
            }
            let _ = g.verifier(&format!("n{}", n), "faux", &format!("10.5.0.{}", n));
        }
        let mut i = tenir(&g.interne);
        g.purger(&mut i, maintenant());
        assert!(i.echecs.len() <= 3);
        assert!(i.echecs_comptes.len() <= 3);
        drop(i);
        let _ = std::fs::remove_dir_all(d);
    }
}
