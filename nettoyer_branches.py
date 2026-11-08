#!/usr/bin/env python3
"""Supprime les branches déjà fusionnées dans origin/main, dépôt par dépôt.

Parcourt tous les dépôts git sous la racine (par défaut ~/Documents). Une branche
(locale ou sur origin) n'est supprimée que si tout son contenu est déjà dans
origin/main : elle en est ancêtre (fusion classique), ou la fusionner dans main
n'apporterait aucun changement (fusion squash ou rebase). Les autres sont gardées.
Demande confirmation pour chaque dépôt.

    python3 nettoyer_branches.py [racine]
"""
import os
import subprocess
import sys

PRINCIPALE = "main"
REF = "origin/" + PRINCIPALE


def git(depot, *args):
    return subprocess.run(["git", "-C", depot, *args], capture_output=True, text=True)


def depots(racine):
    for dossier, sous, _ in os.walk(racine):
        # .git en dossier seulement : un .git fichier est un worktree, déjà couvert par son dépôt
        if os.path.isdir(os.path.join(dossier, ".git")):
            yield dossier
        sous[:] = sorted(d for d in sous if not d.startswith(".") and d != "node_modules")


def fusionnee(depot, ref):
    """Vrai si tout le contenu de ref est déjà dans origin/main."""
    if git(depot, "merge-base", "--is-ancestor", ref, REF).returncode == 0:
        return True
    fusion = git(depot, "merge-tree", "--write-tree", REF, ref)  # code 1 = conflit
    arbre_main = git(depot, "rev-parse", REF + "^{tree}").stdout.strip()
    return fusion.returncode == 0 and fusion.stdout.split()[0] == arbre_main


def branches(depot, prefixe):
    sortie = git(depot, "for-each-ref", "--format=%(refname)\t%(objectname:short)", prefixe).stdout
    lignes = (l.split("\t") for l in sortie.splitlines())
    return {ref[len(prefixe):]: sha for ref, sha in lignes if ref[len(prefixe):] not in (PRINCIPALE, "HEAD")}


def confirmer(question):
    try:
        return input(question + " [o/N] ").strip().lower() in ("o", "oui")
    except EOFError:
        return False


def nettoyer(depot):
    print(f"\n== {depot}")
    fetch = git(depot, "fetch", "--prune", "origin")
    if fetch.returncode:
        print("  fetch impossible, dépôt ignoré :", fetch.stderr.strip())
        return
    if git(depot, "rev-parse", "--verify", "-q", REF).returncode:
        print(f"  pas de {REF}, dépôt ignoré")
        return
    courante = git(depot, "branch", "--show-current").stdout.strip()
    locales = {n: s for n, s in branches(depot, "refs/heads/").items() if n != courante}
    distantes = branches(depot, "refs/remotes/origin/")
    a_suppr = [("locale", n, s) for n, s in locales.items() if fusionnee(depot, "refs/heads/" + n)]
    a_suppr += [("origin", n, s) for n, s in distantes.items() if fusionnee(depot, "refs/remotes/origin/" + n)]
    gardees = len(locales) + len(distantes) - len(a_suppr)
    if not a_suppr:
        print(f"  rien à supprimer ({gardees} branche(s) non fusionnée(s) gardée(s))")
        return
    for ou, nom, sha in a_suppr:
        print(f"  fusionnée  {ou:6} {nom}  ({sha})")
    print(f"  {gardees} branche(s) non fusionnée(s) gardée(s)")
    if not confirmer(f"  Supprimer ces {len(a_suppr)} branche(s) ?"):
        return
    for ou, nom, sha in a_suppr:
        # -D : la fusion est déjà vérifiée ci-dessus, -d refuserait les squash
        r = git(depot, "branch", "-D", nom) if ou == "locale" else git(depot, "push", "origin", "--delete", nom)
        etat = "supprimée" if r.returncode == 0 else "ÉCHEC : " + r.stderr.strip().splitlines()[-1]
        # le SHA permet de la recréer : git branch <nom> <sha>
        print(f"  {etat}  {ou:6} {nom}  ({sha})")


def main(argv):
    racine = os.path.abspath(os.path.expanduser(argv[1] if len(argv) > 1 else "~/Documents"))
    for depot in depots(racine):
        nettoyer(depot)


if __name__ == "__main__":
    main(sys.argv)
