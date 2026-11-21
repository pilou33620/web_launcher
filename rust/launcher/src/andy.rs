//! Suivi d'Andy : les pesées et évènements ajoutés depuis la page, gardés dans andy.local.json
//! (propre à ce Pi, comme config.local.json). L'historique médical est dans
//! andy_donnees.local.json, sur le Pi seulement (jamais sur GitHub : le dépôt est public). Servi
//! par /api/andy/donnees, après connexion.
//!
//! Il était autrefois dans web/js/andy_donnees.json : s'il manque, il est repris de là, ou de
//! l'historique git (commit COMMIT_ANCIEN), une seule fois.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use chrono::NaiveDate;
use serde_json::{json, Value};

use crate::{alea, ecrire_atomique, ecrire_json, sortie, tenir};

const ANCIEN: &str = "web/js/andy_donnees.json";
pub const COMMIT_ANCIEN: &str = "6b778c7"; // dernier commit du dépôt qui contenait ANCIEN
const POIDS_MIN: f64 = 0.5;
const POIDS_MAX: f64 = 100.0;
const MAX_PESEES: usize = 5000;
const TYPES_EVENEMENT: [&str; 8] = ["medicament", "consultation", "vaccin", "vermifuge", "antiparasitaire", "soin", "osteo", "rdv"];

fn naissance() -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 11, 30).expect("date valide")
}

pub struct Andy {
    pub fichier: PathBuf,
    pub historique: PathBuf,
    pub racine: PathBuf,
    pub commit_ancien: String,
    verrou: Mutex<()>, // le serveur traite chaque requête dans son propre fil
}

/// L'id d'une entrée : 8 chiffres hexadécimaux aléatoires (uuid4().hex[:8]).
fn nouvel_id() -> String {
    alea(4).iter().map(|b| format!("{:02x}", b)).collect()
}

fn aujourdhui() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// date.fromisoformat : « AAAA-MM-JJ » seulement.
fn jour(date: &Value, limite: NaiveDate, hors: &str) -> Result<NaiveDate, String> {
    let texte = date.as_str().filter(|t| t.len() == 10).ok_or("Date invalide.")?;
    let j = NaiveDate::parse_from_str(texte, "%Y-%m-%d").map_err(|_| "Date invalide.".to_string())?;
    if j < naissance() || j > limite {
        return Err(hors.into());
    }
    Ok(j)
}

impl Andy {
    pub fn new(racine: &Path) -> Andy {
        Andy {
            fichier: racine.join("andy.local.json"),
            historique: racine.join("andy_donnees.local.json"),
            racine: racine.to_path_buf(),
            commit_ancien: COMMIT_ANCIEN.into(),
            verrou: Mutex::new(()),
        }
    }

    /// (pesées, évènements) valides ; fichier absent ou abîmé : rien (jamais d'erreur).
    fn lire_tout(&self) -> (Vec<Value>, Vec<Value>) {
        let brut: Value = std::fs::read(&self.fichier).ok().and_then(|o| serde_json::from_slice(&o).ok()).unwrap_or(Value::Null);
        let liste = |cle: &str| brut.get(cle).and_then(Value::as_array).cloned().unwrap_or_default();
        let ok = |e: &Value| e.get("id").is_some_and(Value::is_string) && e.get("date").is_some_and(Value::is_string);
        let pesees = liste("pesees").into_iter().filter(|p| ok(p) && p.get("poids").is_some_and(|x| x.is_number() || x.is_boolean())).collect();
        let evenements = liste("evenements")
            .into_iter()
            .filter(|e| {
                ok(e) && e.get("type").and_then(Value::as_str).is_some_and(|t| TYPES_EVENEMENT.contains(&t)) && e.get("titre").is_some_and(Value::is_string)
            })
            .collect();
        (pesees, evenements)
    }

    fn ecrire(&self, pesees: Vec<Value>, evenements: Vec<Value>) -> Result<(), String> {
        ecrire_json(&self.fichier, &json!({"pesees": pesees, "evenements": evenements}), 1, false, false).map_err(|e| e.to_string())
    }

    fn trier(mut v: Vec<Value>) -> Vec<Value> {
        v.sort_by(|a, b| a["date"].as_str().cmp(&b["date"].as_str()));
        v
    }

    pub fn lire(&self) -> Vec<Value> {
        let _v = tenir(&self.verrou);
        Self::trier(self.lire_tout().0)
    }

    pub fn lire_evenements(&self) -> Vec<Value> {
        let _v = tenir(&self.verrou);
        Self::trier(self.lire_tout().1)
    }

    /// Ajoute une pesée et la rend. Err (message affichable) si la date ou le poids est invalide.
    pub fn ajouter(&self, date: &Value, poids: &Value, aujourdhui_: Option<NaiveDate>) -> Result<Value, String> {
        // un jour de marge sur « aujourd'hui » : l'appareil et le Pi ne sont pas forcément dans le même fuseau
        let limite = aujourdhui_.unwrap_or_else(aujourdhui) + chrono::Duration::days(1);
        let j = jour(date, limite, "La date doit être entre la naissance d'Andy et aujourd'hui.")?;
        let p = match poids {
            Value::Number(n) => n.as_f64().filter(|x| (POIDS_MIN..=POIDS_MAX).contains(x)),
            _ => None,
        }
        .ok_or_else(|| format!("Le poids doit être entre {} et {} kg.", POIDS_MIN, POIDS_MAX))?;
        let entree = json!({"id": nouvel_id(), "date": j.format("%Y-%m-%d").to_string(), "poids": (p * 100.0).round() / 100.0});
        let _v = tenir(&self.verrou);
        let (mut pesees, evenements) = self.lire_tout();
        if pesees.len() >= MAX_PESEES {
            return Err("Trop de pesées enregistrées.".into());
        }
        pesees.push(entree.clone());
        self.ecrire(pesees, evenements)?;
        Ok(entree)
    }

    /// Ajoute un évènement (médicament, visite…), passé ou à venir, et le rend.
    pub fn ajouter_evenement(&self, date: &Value, type_: &Value, titre: &Value, note: &Value, aujourdhui_: Option<NaiveDate>) -> Result<Value, String> {
        let limite = aujourdhui_.unwrap_or_else(aujourdhui) + chrono::Duration::days(730);
        let j = jour(date, limite, "Date hors des limites.")?;
        let type_ = type_.as_str().filter(|t| TYPES_EVENEMENT.contains(t)).ok_or("Type d'évènement inconnu.")?;
        let titre = titre.as_str().unwrap_or("").trim().to_string();
        if !(1..=100).contains(&titre.chars().count()) {
            return Err("Le titre est obligatoire (100 caractères au plus).".into());
        }
        let note = note.as_str().unwrap_or("").trim().to_string();
        if note.chars().count() > 500 {
            return Err("La note est trop longue (500 caractères au plus).".into());
        }
        let entree = json!({"id": nouvel_id(), "date": j.format("%Y-%m-%d").to_string(), "type": type_, "titre": titre, "note": note});
        let _v = tenir(&self.verrou);
        let (pesees, mut evenements) = self.lire_tout();
        if evenements.len() >= MAX_PESEES {
            return Err("Trop d'évènements enregistrés.".into());
        }
        evenements.push(entree.clone());
        self.ecrire(pesees, evenements)?;
        Ok(entree)
    }

    /// Vrai si la pesée ou l'évènement existait.
    pub fn supprimer(&self, identifiant: &str) -> bool {
        let _v = tenir(&self.verrou);
        let (mut pesees, mut evenements) = self.lire_tout();
        let avant = pesees.len() + evenements.len();
        pesees.retain(|e| e["id"] != identifiant);
        evenements.retain(|e| e["id"] != identifiant);
        if pesees.len() + evenements.len() == avant {
            return false;
        }
        self.ecrire(pesees, evenements).is_ok()
    }

    /// Contenu de l'ancien fichier suivi par git (encore présent, ou dans l'historique), ou None.
    fn reprendre(&self) -> Option<Vec<u8>> {
        if let Ok(o) = std::fs::read(self.racine.join(ANCIEN)) {
            return Some(o);
        }
        let mut c = Command::new("git");
        c.arg("-C").arg(&self.racine).arg("show").arg(format!("{}:{}", self.commit_ancien, ANCIEN));
        let r = sortie(c, Duration::from_secs(30)).ok()?;
        (r.status.success() && !r.stdout.is_empty()).then_some(r.stdout)
    }

    /// L'historique médical, ou None s'il n'existe pas sur ce Pi.
    pub fn historique(&self) -> Option<Value> {
        let _v = tenir(&self.verrou);
        if !self.historique.exists() {
            let brut = self.reprendre()?;
            serde_json::from_slice::<Value>(&brut).ok()?;
            ecrire_atomique(&self.historique, &brut, true).ok()?;
        }
        serde_json::from_slice(&std::fs::read(&self.historique).ok()?).ok()
    }
}

#[cfg(test)]
mod essais {
    use super::*;

    fn andy(nom: &str) -> (Andy, PathBuf) {
        let d = std::env::temp_dir().join(format!("lanceur-andy-{}-{}", nom, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut a = Andy::new(&d);
        a.racine = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        (a, d)
    }

    fn auj() -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(2026, 11, 5)
    }

    #[test]
    fn validation() {
        let (a, d) = andy("validation");
        for (date, poids) in [
            (json!("2025-11-29"), json!(5)),
            (json!("2026-11-07"), json!(5)),
            (json!("2026-13-01"), json!(5)),
            (json!("hier"), json!(5)),
            (json!(null), json!(5)),
            (json!("2026-11-05"), json!(0.4)),
            (json!("2026-11-05"), json!(100.1)),
            (json!("2026-11-05"), json!(true)),
            (json!("2026-11-05"), json!("13")),
            (json!("2026-11-05"), json!(null)),
        ] {
            assert!(a.ajouter(&date, &poids, auj()).is_err(), "{date} {poids}");
        }
        assert!(a.lire().is_empty());
        a.ajouter(&json!("2025-11-30"), &json!(0.5), auj()).unwrap();
        a.ajouter(&json!("2026-11-06"), &json!(100), auj()).unwrap(); // un jour de marge pour le fuseau
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn ajouter_lire_supprimer() {
        let (a, d) = andy("pesees");
        let b = a.ajouter(&json!("2026-11-05"), &json!(14.236), auj()).unwrap();
        let x = a.ajouter(&json!("2026-10-07"), &json!(13.6), auj()).unwrap();
        assert_eq!(b["poids"], json!(14.24));
        let dates: Vec<Value> = a.lire().iter().map(|p| p["date"].clone()).collect();
        assert_eq!(dates, vec![json!("2026-10-07"), json!("2026-11-05")]);
        assert!(!a.supprimer("inconnu"));
        assert!(a.supprimer(x["id"].as_str().unwrap()));
        assert_eq!(a.lire().len(), 1);
        // fichier abîmé ou entrées bancales : ignorés, pas de plantage
        std::fs::write(&a.fichier, r#"{"pesees": [{"id": "x", "date": "2026-01-01", "poids": 5}, {"id": 1}, 7, {"id": "y", "date": "d", "poids": "p"}]}"#).unwrap();
        assert_eq!(a.lire()[0]["id"], "x");
        assert_eq!(a.lire().len(), 1);
        std::fs::write(&a.fichier, "{pas du json").unwrap();
        assert!(a.lire().is_empty());
        assert_eq!(a.ajouter(&json!("2026-01-01"), &json!(5), auj()).unwrap()["poids"], json!(5.0));
        // le fichier de la version Python se relit tel quel
        std::fs::write(&a.fichier, "{\n \"pesees\": [\n  {\n   \"id\": \"f1983aaa\",\n   \"date\": \"2026-11-06\",\n   \"poids\": 14.9\n  }\n ]\n}").unwrap();
        assert_eq!(a.lire(), vec![json!({"id": "f1983aaa", "date": "2026-11-06", "poids": 14.9})]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn evenements() {
        let (a, d) = andy("evenements");
        let n501 = "n".repeat(501);
        for (date, t, titre, note) in [
            ("2026-01-01", "inconnu", "x", ""),
            ("2026-01-01", "medicament", "  ", ""),
            ("2026-01-01", "medicament", "x", n501.as_str()),
            ("2025-01-01", "medicament", "x", ""),
            ("2029-01-01", "rdv", "x", ""),
            ("pas une date", "rdv", "x", ""),
        ] {
            assert!(a.ajouter_evenement(&json!(date), &json!(t), &json!(titre), &json!(note), auj()).is_err());
        }
        let m = a.ajouter_evenement(&json!("2026-11-01"), &json!("medicament"), &json!(" Nexgard "), &json!("1 comprimé"), auj()).unwrap();
        let v = a.ajouter_evenement(&json!("2026-12-01"), &json!("rdv"), &json!("Visite véto"), &Value::Null, auj()).unwrap();
        a.ajouter(&json!("2026-11-02"), &json!(14), auj()).unwrap();
        let titres: Vec<Value> = a.lire_evenements().iter().map(|e| e["titre"].clone()).collect();
        assert_eq!(titres, vec![json!("Nexgard"), json!("Visite véto")]);
        assert_eq!(a.lire().len(), 1); // les pesées ne sont pas écrasées
        assert!(a.supprimer(m["id"].as_str().unwrap()));
        assert_eq!(a.lire_evenements()[0]["id"], v["id"]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn historique_repris_de_git() {
        let (mut a, d) = andy("historique");
        // dépôt cloné sans profondeur (CI) : le vieux commit peut manquer, l'essai se borne alors au refus
        if let Some(evenements) = a.historique() {
            let liste = evenements.as_array().unwrap().clone();
            assert!(liste.len() > 30);
            let dates: Vec<&str> = liste.iter().map(|e| e["date"].as_str().unwrap()).collect();
            let mut triees = dates.clone();
            triees.sort();
            assert_eq!(dates, triees);
            assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&a.historique).unwrap().permissions()) & 0o777, 0o600);
            std::fs::remove_file(&a.historique).unwrap();
        }
        a.commit_ancien = "0000000".into();
        assert!(a.historique().is_none());
        let _ = std::fs::remove_dir_all(d);
    }
}
