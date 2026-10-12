"""Lecture du catalogue (outils.json) et des réglages propres à ce Raspberry Pi (config.local.json)."""

import json
import os

RACINE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CATALOGUE = os.path.join(RACINE, "outils.json")
LOCAL = os.path.join(RACINE, "config.local.json")
ETAT = os.path.join(RACINE, "etat.local.json")
JOURNAUX = os.path.join(RACINE, "journaux")


def _lire(chemin):
    try:
        with open(chemin, encoding="utf-8") as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


def charger(catalogue=CATALOGUE, local=LOCAL):
    """Le catalogue, complété par config.local.json.

    config.local.json peut remplacer les réglages généraux (port, dossier_outils...)
    et, dans « outils », les champs d'un outil désigné par son id ; un id inconnu
    ajoute un outil."""
    conf = _lire(catalogue)
    ajout = _lire(local)
    outils = {o["id"]: dict(o) for o in conf.get("outils", [])}
    ordre = [o["id"] for o in conf.get("outils", [])]
    for o in ajout.pop("outils", []):
        if o.get("id") in outils:
            outils[o["id"]].update(o)
        elif o.get("id"):
            outils[o["id"]] = dict(o)
            ordre.append(o["id"])
    conf.update(ajout)
    conf["outils"] = [outils[i] for i in ordre if not outils[i].get("masque")]
    conf["outils_masques"] = [outils[i] for i in ordre if outils[i].get("masque")]
    conf["ordre"] = ordre
    conf["dossier_outils"] = os.path.abspath(os.path.expanduser(conf.get("dossier_outils", "~/Documents")))
    return conf


def regler_outil(oid, chemin=LOCAL, **champs):
    """Change des réglages d'un outil dans config.local.json (le reste du fichier est gardé).
    Un champ à None est retiré : la valeur du catalogue revient."""
    local = _lire(chemin)
    outils = local.setdefault("outils", [])
    entree = next((o for o in outils if o.get("id") == oid), None)
    if entree is None:
        entree = {"id": oid}
        outils.append(entree)
    for cle, valeur in champs.items():
        if valeur is None:
            entree.pop(cle, None)
        else:
            entree[cle] = valeur
    if len(entree) == 1:                    # plus que l'id : rien à remplacer
        outils.remove(entree)
    if not outils:
        del local["outils"]
    temporaire = chemin + ".tmp"
    with open(temporaire, "w", encoding="utf-8") as f:
        json.dump(local, f, ensure_ascii=False, indent=2)
        f.write("\n")
    os.replace(temporaire, chemin)


def lire_etat():
    return _lire(ETAT)


def ecrire_etat(etat):
    temporaire = ETAT + ".tmp"
    with open(temporaire, "w", encoding="utf-8") as f:
        json.dump(etat, f, ensure_ascii=False, indent=1)
    os.replace(temporaire, ETAT)
