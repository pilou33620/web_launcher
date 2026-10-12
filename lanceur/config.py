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
    conf["dossier_outils"] = os.path.abspath(os.path.expanduser(conf.get("dossier_outils", "~/Documents")))
    return conf


def lire_etat():
    return _lire(ETAT)


def ecrire_etat(etat):
    temporaire = ETAT + ".tmp"
    with open(temporaire, "w", encoding="utf-8") as f:
        json.dump(etat, f, ensure_ascii=False, indent=1)
    os.replace(temporaire, ETAT)
