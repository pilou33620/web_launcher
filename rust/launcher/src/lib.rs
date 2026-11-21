//! Web Launcher : allume, éteint, installe et met à jour les web tools du Raspberry Pi.
//! Voir README.md. Mêmes fichiers, mêmes routes et mêmes JSON que l'ancienne version Python.

pub mod andy;
pub mod claude;
pub mod compte;
pub mod config;
pub mod console;
pub mod depot;
pub mod gestionnaire;
pub mod nettoyer;
pub mod relais;
pub mod reseau;
pub mod serveur;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Le dossier du launcher (outils.json, *.local.json, web/) : le dossier courant s'il contient
/// outils.json, sinon le premier dossier au-dessus de l'exécutable qui le contient.
pub fn racine() -> &'static Path {
    static R: OnceLock<PathBuf> = OnceLock::new();
    R.get_or_init(|| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let ici = |d: &Path| d.join("outils.json").is_file();
        if ici(&cwd) {
            return cwd;
        }
        let exe = std::env::current_exe().ok().and_then(|e| std::fs::canonicalize(e).ok());
        exe.and_then(|e| e.ancestors().find(|d| ici(d)).map(Path::to_path_buf)).unwrap_or(cwd)
    })
}

/// time.time() de Python.
pub fn maintenant() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// time.strftime de Python, à l'heure locale.
pub fn heure(format: &str) -> String {
    chrono::Local::now().format(format).to_string()
}

/// Un Mutex empoisonné (panique dans un autre fil) se reprend : l'état reste utilisable.
pub fn tenir<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `n` octets aléatoires du système.
pub fn alea(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("générateur aléatoire du système");
    b
}

/// Le JSON d'un fichier ; absent : `{}` (comme config._lire de Python). Illisible : erreur.
pub fn lire_json(chemin: &Path) -> io::Result<Value> {
    match std::fs::read(chemin) {
        Ok(o) => serde_json::from_slice(&o).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Value::Object(Default::default())),
        Err(e) => Err(e),
    }
}

/// Écriture atomique (temporaire puis renommage), avec l'indentation de la version Python.
/// `prive` : droits 0600 (comptes, sessions, historique d'Andy).
pub fn ecrire_json(chemin: &Path, v: &Value, indent: usize, prive: bool, fin_ligne: bool) -> io::Result<()> {
    let mut texte = Vec::new();
    let espaces = vec![b' '; indent];
    let mut ser = serde_json::Serializer::with_formatter(&mut texte, serde_json::ser::PrettyFormatter::with_indent(&espaces));
    serde::Serialize::serialize(v, &mut ser).map_err(io::Error::other)?;
    if fin_ligne {
        texte.push(b'\n');
    }
    ecrire_atomique(chemin, &texte, prive)
}

pub fn ecrire_atomique(chemin: &Path, octets: &[u8], prive: bool) -> io::Result<()> {
    let temporaire = PathBuf::from(format!("{}.tmp", chemin.display()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    if prive {
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    }
    let ecrit = o.open(&temporaire).and_then(|mut f| f.write_all(octets)).and_then(|_| std::fs::rename(&temporaire, chemin));
    if ecrit.is_err() {
        let _ = std::fs::remove_file(&temporaire);
    }
    ecrit
}

/// Vérité d'une valeur JSON comme bool() de Python : null, false, 0, "", [] et {} sont faux.
pub fn vrai(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|x| x != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// str() de Python sur une valeur reçue en JSON (absente : `defaut`), coupée à `max` caractères.
pub fn texte(v: Option<&Value>, defaut: &str, max: usize) -> String {
    let s = match v {
        None => defaut.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) => "None".into(),
        Some(Value::Bool(b)) => (if *b { "True" } else { "False" }).into(),
        Some(autre) => autre.to_string(),
    };
    s.chars().take(max).collect()
}

/// Un programme dans le PATH.
pub fn dans_le_path(nom: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(nom)).find(|p| p.is_file())
}

pub enum Echec {
    /// Le programme ne se lance pas (absent...).
    Lancement(io::Error),
    /// Délai dépassé : le programme a été tué.
    Delai,
}

/// Lance une commande sans entrée, sorties capturées, tuée si elle dépasse `delai`.
pub fn sortie(mut c: Command, delai: Duration) -> Result<Output, Echec> {
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let enfant = c.spawn().map_err(Echec::Lancement)?;
    let pid = enfant.id() as i32;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(enfant.wait_with_output());
    });
    match rx.recv_timeout(delai) {
        Ok(r) => r.map_err(Echec::Lancement),
        Err(_) => {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
            Err(Echec::Delai)
        }
    }
}

#[cfg(test)]
mod essais {
    use super::*;
    use serde_json::json;

    #[test]
    fn verite_et_texte_comme_python() {
        for (v, b) in [(json!(null), false), (json!(0), false), (json!(""), false), (json!([]), false), (json!(1), true), (json!("x"), true)] {
            assert_eq!(vrai(&v), b, "{v}");
        }
        assert_eq!(texte(None, "", 10), "");
        assert_eq!(texte(Some(&json!(5)), "", 10), "5");
        assert_eq!(texte(Some(&json!("été et hiver")), "", 3), "été");
    }

    #[test]
    fn json_indente_comme_python() {
        let d = std::env::temp_dir().join(format!("lanceur-json-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.json");
        ecrire_json(&f, &json!({"a": [1], "b": "é"}), 1, true, false).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\n \"a\": [\n  1\n ],\n \"b\": \"é\"\n}");
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&f).unwrap().permissions()) & 0o777, 0o600);
        assert_eq!(lire_json(&d.join("absent.json")).unwrap(), json!({}));
        std::fs::write(&f, "{pas du json").unwrap();
        assert!(lire_json(&f).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
