//! Opérations git d'un web tool : version, recherche et application des mises à jour, clonage.
//!
//! Jamais de fusion ni d'écrasement : « git pull --ff-only » seulement, et rien si des fichiers
//! suivis ont été modifiés à la main. Git ne demande jamais de mot de passe (un dépôt privé doit
//! être accessible sans saisie : clé SSH ou jeton enregistré). Toute erreur est un message lisible,
//! affiché tel quel dans l'interface.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::{sortie, Echec};

pub const DELAI: u64 = 120; // s

pub fn git(dossier: &Path, args: &[&str], delai: u64, verifier: bool) -> Result<String, String> {
    let mut c = Command::new("git");
    c.args(args).current_dir(dossier).env("GIT_TERMINAL_PROMPT", "0").env("LC_ALL", "C");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let r = match sortie(c, Duration::from_secs(delai)) {
        Ok(r) => r,
        Err(Echec::Lancement(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("git n'est pas installé (sudo apt install git).".into())
        }
        Err(Echec::Lancement(e)) => return Err(format!("git : {}", e)),
        Err(Echec::Delai) => return Err("git ne répond pas (pas d'internet ?).".into()),
    };
    let stdout = String::from_utf8_lossy(&r.stdout).trim().to_string();
    if verifier && !r.status.success() {
        let stderr = String::from_utf8_lossy(&r.stderr).to_string();
        return Err(expliquer(if stderr.trim().is_empty() { &stdout } else { &stderr }));
    }
    Ok(stdout)
}

/// Dernière ligne utile de git, avec une piste pour les erreurs fréquentes.
pub fn expliquer(sortie: &str) -> String {
    let texte = sortie.trim();
    let bas = texte.to_lowercase();
    if ["authentication", "could not read username", "permission denied (publickey)", "repository not found"]
        .iter()
        .any(|m| bas.contains(m))
    {
        return "GitHub refuse l'accès (dépôt privé ?) : connecter le Raspberry Pi à GitHub, voir README « Dépôts privés ».".into();
    }
    if bas.contains("could not resolve host") || bas.contains("unable to access") {
        return "GitHub injoignable (pas d'internet ?).".into();
    }
    texte.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("échec de git").to_string()
}

pub fn est_depot(dossier: &Path) -> bool {
    dossier.join(".git").is_dir()
}

/// {"commit", "date", "sujet"} du commit courant, ou null.
pub fn version(dossier: &Path) -> Value {
    if !est_depot(dossier) {
        return Value::Null;
    }
    match git(dossier, &["log", "-1", "--format=%h%x1f%cI%x1f%s"], 10, true) {
        Ok(s) => {
            let mut m = s.split('\x1f').map(str::to_string).chain(std::iter::repeat(String::new()));
            json!({"commit": m.next(), "date": m.next(), "sujet": m.next()})
        }
        Err(_) => Value::Null,
    }
}

/// Fichiers suivis modifiés localement (ils bloqueraient la mise à jour).
pub fn fichiers_modifies(dossier: &Path) -> Result<Vec<String>, String> {
    Ok(git(dossier, &["diff", "--name-only", "HEAD"], 20, true)?.lines().map(str::to_string).collect())
}

pub struct Verification {
    pub nombre: u64,
    pub nouveautes: Vec<String>,
    pub modifies: Vec<String>,
}

/// git fetch puis compare avec la branche suivie.
pub fn verifier(dossier: &Path) -> Result<Verification, String> {
    if !est_depot(dossier) {
        return Err("pas un dépôt git (installé sans « git clone »).".into());
    }
    git(dossier, &["fetch", "--quiet"], DELAI, true)?;
    if git(dossier, &["rev-parse", "--abbrev-ref", "@{u}"], 10, true).is_err() {
        return Err("aucune branche distante suivie.".into());
    }
    let nombre = git(dossier, &["rev-list", "--count", "HEAD..@{u}"], 20, true)?.parse().unwrap_or(0);
    let nouveautes = if nombre > 0 {
        git(dossier, &["log", "--format=%s", "-10", "HEAD..@{u}"], 20, true)?.lines().map(str::to_string).collect()
    } else {
        Vec::new()
    };
    Ok(Verification { nombre, nouveautes, modifies: fichiers_modifies(dossier)? })
}

/// Avance la copie locale. Rend la liste des fichiers changés (vide : déjà à jour).
pub fn appliquer(dossier: &Path) -> Result<Vec<String>, String> {
    let modifies = fichiers_modifies(dossier)?;
    if !modifies.is_empty() {
        return Err(format!(
            "fichiers modifiés sur le Raspberry Pi, mise à jour bloquée : {}{}",
            modifies.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            if modifies.len() > 5 { " …" } else { "" }
        ));
    }
    let avant = git(dossier, &["rev-parse", "HEAD"], 10, true)?;
    git(dossier, &["pull", "--ff-only", "--quiet"], DELAI, true)?;
    let apres = git(dossier, &["rev-parse", "HEAD"], 10, true)?;
    if avant == apres {
        return Ok(Vec::new());
    }
    Ok(git(dossier, &["diff", "--name-only", &avant, &apres], 20, true)?.lines().map(str::to_string).collect())
}

pub fn cloner(url: &str, cible: &Path) -> Result<(), String> {
    let parent = cible.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporaire = PathBuf::from(format!("{}.clonage", cible.display()));
    if temporaire.exists() {
        let _ = std::fs::remove_dir_all(&temporaire);
    }
    let t = temporaire.to_string_lossy().into_owned();
    if let Err(e) = git(parent, &["clone", "--quiet", url, &t], 900, true) {
        let _ = std::fs::remove_dir_all(&temporaire);
        return Err(e);
    }
    std::fs::rename(&temporaire, cible).map_err(|e| e.to_string())
}

#[cfg(test)]
mod essais {
    use super::*;

    #[test]
    fn messages_de_git() {
        assert!(expliquer("remote: Repository not found.\nfatal: ...").starts_with("GitHub refuse"));
        assert_eq!(expliquer("fatal: unable to access 'https://x'"), "GitHub injoignable (pas d'internet ?).");
        assert_eq!(expliquer("a\n\nderniere\n"), "derniere");
        assert_eq!(expliquer(""), "échec de git");
    }
}
