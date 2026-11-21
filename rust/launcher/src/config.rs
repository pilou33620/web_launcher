//! Lecture du catalogue (outils.json) et des réglages propres à ce Raspberry Pi (config.local.json).

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::{ecrire_json, lire_json, vrai};

/// Les fichiers du launcher (changés par les bancs d'essai pour ne pas toucher à ceux du Pi).
#[derive(Clone, Debug)]
pub struct Chemins {
    pub racine: PathBuf,
    pub catalogue: PathBuf,
    pub local: PathBuf,
    pub etat: PathBuf,
    pub journaux: PathBuf,
}

impl Chemins {
    pub fn de(racine: &Path) -> Chemins {
        Chemins {
            racine: racine.to_path_buf(),
            catalogue: racine.join("outils.json"),
            local: racine.join("config.local.json"),
            etat: racine.join("etat.local.json"),
            journaux: racine.join("journaux"),
        }
    }
}

/// os.path.abspath(os.path.expanduser(texte)).
pub fn chemin_absolu(texte: &str) -> PathBuf {
    let maison = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let p = if texte == "~" {
        maison
    } else if let Some(reste) = texte.strip_prefix("~/") {
        maison.join(reste)
    } else {
        PathBuf::from(texte)
    };
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    }
}

fn objet(chemin: &Path) -> Result<Map<String, Value>, String> {
    match lire_json(chemin) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(format!("{} : un objet JSON est attendu", chemin.display())),
        Err(e) => Err(format!("{} illisible : {}", chemin.display(), e)),
    }
}

/// Le catalogue, complété par config.local.json.
///
/// config.local.json peut remplacer les réglages généraux (port, dossier_outils...) et, dans
/// « outils », les champs d'un outil désigné par son id ; un id inconnu ajoute un outil.
pub fn charger(catalogue: &Path, local: &Path) -> Result<Map<String, Value>, String> {
    let mut conf = objet(catalogue)?;
    let mut ajout = objet(local)?;
    let mut outils: Map<String, Value> = Map::new();
    let mut ordre: Vec<String> = Vec::new();
    for o in conf.get("outils").and_then(Value::as_array).cloned().unwrap_or_default() {
        if let Some(id) = o.get("id").and_then(Value::as_str) {
            ordre.push(id.to_string());
            outils.insert(id.to_string(), o);
        }
    }
    for o in ajout.remove("outils").and_then(|v| v.as_array().cloned()).unwrap_or_default() {
        let Some(champs) = o.as_object() else { continue };
        let id = champs.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        if let Some(Value::Object(connu)) = outils.get_mut(&id) {
            for (k, v) in champs {
                connu.insert(k.clone(), v.clone());
            }
        } else if !id.is_empty() {
            ordre.push(id.clone());
            outils.insert(id, o.clone());
        }
    }
    for (k, v) in ajout {
        conf.insert(k, v);
    }
    let masque = |id: &String| vrai(&outils[id]["masque"]);
    conf.insert("outils".into(), Value::Array(ordre.iter().filter(|i| !masque(i)).map(|i| outils[i].clone()).collect()));
    conf.insert("outils_masques".into(), Value::Array(ordre.iter().filter(|i| masque(i)).map(|i| outils[i].clone()).collect()));
    conf.insert("ordre".into(), Value::Array(ordre.iter().map(|i| Value::String(i.clone())).collect()));
    let dossier = conf.get("dossier_outils").and_then(Value::as_str).unwrap_or("~/Documents").to_string();
    conf.insert("dossier_outils".into(), Value::String(chemin_absolu(&dossier).to_string_lossy().into_owned()));
    Ok(conf)
}

/// Change des réglages d'un outil dans config.local.json (le reste du fichier est gardé).
/// Un champ à None est retiré : la valeur du catalogue revient.
pub fn regler_outil(oid: &str, chemin: &Path, champs: &[(&str, Option<Value>)]) -> io::Result<()> {
    let mut local = match lire_json(chemin)? {
        Value::Object(m) => m,
        _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "config.local.json : un objet JSON est attendu")),
    };
    let mut outils = local.remove("outils").and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let rang = match outils.iter().position(|o| o.get("id").and_then(Value::as_str) == Some(oid)) {
        Some(i) => i,
        None => {
            outils.push(serde_json::json!({ "id": oid }));
            outils.len() - 1
        }
    };
    if let Some(entree) = outils[rang].as_object_mut() {
        for (cle, valeur) in champs {
            match valeur {
                Some(v) => {
                    entree.insert(cle.to_string(), v.clone());
                }
                None => {
                    entree.remove(*cle);
                }
            }
        }
    }
    if outils[rang].as_object().map(|o| o.len() <= 1).unwrap_or(true) {
        outils.remove(rang); // plus que l'id : rien à remplacer
    }
    if !outils.is_empty() {
        local.insert("outils".into(), Value::Array(outils));
    }
    ecrire_json(chemin, &Value::Object(local), 2, false, true)
}

#[cfg(test)]
mod essais {
    use super::*;
    use serde_json::json;

    fn dossier(nom: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lanceur-config-{}-{}", nom, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn config_locale() {
        let d = dossier("locale");
        let (cat, loc) = (d.join("o.json"), d.join("l.json"));
        std::fs::write(&cat, json!({"port": 80, "outils": [{"id": "a", "port": 1}, {"id": "b", "port": 2}]}).to_string()).unwrap();
        std::fs::write(
            &loc,
            json!({"port": 8090, "dossier_outils": "~/x",
                   "outils": [{"id": "a", "port": 9}, {"id": "b", "masque": true}, {"id": "c", "port": 3}]})
            .to_string(),
        )
        .unwrap();
        let conf = charger(&cat, &loc).unwrap();
        assert_eq!(conf["port"], 8090);
        let outils: Vec<(String, u64)> =
            conf["outils"].as_array().unwrap().iter().map(|o| (o["id"].as_str().unwrap().to_string(), o["port"].as_u64().unwrap())).collect();
        assert_eq!(outils, vec![("a".to_string(), 9), ("c".to_string(), 3)]);
        assert_eq!(conf["outils_masques"][0]["id"], "b");
        assert_eq!(conf["dossier_outils"], chemin_absolu("~/x").to_string_lossy().as_ref());
        // absent : comme un fichier vide ; abîmé : refusé, jamais pris pour vide
        assert!(charger(&cat, &d.join("absent.json")).is_ok());
        std::fs::write(&loc, "{abîmé").unwrap();
        assert!(charger(&cat, &loc).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn reglage_d_un_outil() {
        let d = dossier("reglage");
        let loc = d.join("l.json");
        std::fs::write(&loc, r#"{"verification_maj_heures": 12}"#).unwrap();
        regler_outil("a", &loc, &[("port", Some(json!(9000))), ("masque", Some(json!(true)))]).unwrap();
        assert_eq!(lire_json(&loc).unwrap(), json!({"verification_maj_heures": 12, "outils": [{"id": "a", "port": 9000, "masque": true}]}));
        regler_outil("a", &loc, &[("port", None), ("masque", None)]).unwrap();
        assert_eq!(lire_json(&loc).unwrap(), json!({"verification_maj_heures": 12}));
        assert!(std::fs::read_to_string(&loc).unwrap().ends_with("}\n"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn catalogue_livre() {
        let cat = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../outils.json");
        let conf = charger(&cat, Path::new("/nulle-part/absent.json")).unwrap();
        let mut ports: Vec<u64> = conf["outils"].as_array().unwrap().iter().map(|o| o["port"].as_u64().unwrap()).collect();
        let n = ports.len();
        ports.sort();
        ports.dedup();
        assert_eq!(ports.len(), n, "deux outils sur le même port");
        for o in conf["outils"].as_array().unwrap() {
            assert!(o["commande"].as_array().unwrap().contains(&json!("{port}")), "{}", o["id"]);
        }
    }
}
