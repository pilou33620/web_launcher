#!/usr/bin/env python3
"""Tous les web tools sont-ils joignables par les deux chemins du « nom Tailscale » ?

- Host : le nom Tailscale exact de ce poste est accepté, un autre .ts.net et un nom inconnu refusés
  (dans le code de chaque outil, et sur l'outil qui tourne : sinon, il est à redémarrer).
- Relais : la page de chaque outil et une de ses ressources passent par /o/<id>/. WEB_CAO, WEB_ANTENNA
  et WEB_3D sont lancés exprès (--local, port libre, sans mise à jour) pour essayer le code du disque.

Une ligne par outil, code de sortie 1 s'il y a un problème.   python3 tests/verif_acces_outils.py
"""
import http.client
import json
import os
import re
import socket
import subprocess
import sys
import tempfile
import threading
import time
import types
import urllib.parse

LANCEUR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, LANCEUR)
sys.dont_write_bytecode = True
from lanceur import compte, relais, reseau, serveur  # noqa: E402

DOCUMENTS = os.path.dirname(LANCEUR)
MODULES = ("appairage.py", "lanceur/appairage.py", "serveur.py", "server.py")
SCRIPTS = {"web_cao": "web_CAO.py", "web_antenna": "web_antenna.py", "web_3d": "web_3D.py"}
SONDE = "import sys,importlib;sys.path.insert(0,'.');m=importlib.import_module(sys.argv[1]);" \
        "print([m.hote_permis(n) for n in sys.argv[2:]])"


def code_hote(dossier, nom):
    """Le contrôle Host écrit dans le code de l'outil (attendu : nom accepté, les deux autres refusés)."""
    for module in MODULES:
        chemin = os.path.join(dossier, module)
        if os.path.exists(chemin) and "def hote_permis" in open(chemin, encoding="utf-8").read():
            ici, fichier = os.path.split(chemin)
            r = subprocess.run([sys.executable, "-c", SONDE, fichier[:-3], nom, "autre.tail1.ts.net", "piege.example"],
                               cwd=ici, capture_output=True, text=True, timeout=30,
                               env=dict(os.environ, PYTHONDONTWRITEBYTECODE="1"))
            return None if r.stdout.strip() == "[True, False, False]" else "code Host faux " + r.stdout.strip()
    return "AUCUN contrôle Host"


def statut(port, chemin="/", **entetes):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=15)
    try:
        c.request("GET", chemin, headers=entetes)
        r = c.getresponse()
        return r.status, r.getheader("Location"), r.read()
    finally:
        c.close()


def hote_vivant(port, nom):
    """Ce que dit l'outil qui tourne."""
    if not reseau.port_ouvert(port):
        return "arrêté"
    if statut(port, Host="piege.example")[0] != 403:
        return "PIÈGE ACCEPTÉ"
    return "à redémarrer" if statut(port, Host="%s:%d" % (nom, port))[0] == 403 else "tourne"


def demarrer(dossier, script):
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    p = subprocess.Popen([sys.executable, script, "--port", str(port), "--local", "--sans-navigateur", "--sans-pause",
                          "--sans-maj"], cwd=dossier, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, env=dict(os.environ, PYTHONDONTWRITEBYTECODE="1"))
    fin = time.time() + 40
    while time.time() < fin and p.poll() is None and not reseau.port_ouvert(port):
        time.sleep(0.2)
    return p, port


def par_relais(port_relais, jeton, id_):
    """Page puis première ressource (.js/.css) de l'outil, relayées. None si tout passe."""
    biscuit = "launcher_session=" + jeton
    chemin = relais.prefixe(id_)
    code, lieu, corps = statut(port_relais, chemin, Cookie=biscuit, Accept="text/html")
    for _ in range(3):
        if code not in (301, 302, 303, 307, 308):
            break
        chemin = urllib.parse.urljoin(chemin, lieu)
        code, lieu, corps = statut(port_relais, chemin, Cookie=biscuit, Accept="text/html")
    if code != 200:
        return "page %s" % code
    if not chemin.startswith(relais.PREFIXE):
        return "sort du relais : " + chemin
    m = re.search(rb"""(?:src|href)=["']([^"'#?:]+\.(?:js|css))""", corps)
    if m:
        res = urllib.parse.urljoin(chemin, m.group(1).decode())
        code = statut(port_relais, res, Cookie=biscuit, Referer="https://pi.ts.net:8443" + chemin)[0]
        if code != 200:
            return "ressource %s : %s" % (res, code)
    return None


def main():
    nom = reseau.nom_tailscale()
    if not nom:
        return print("PROBLÈME : Tailscale ne donne pas le nom de ce poste.") or 1
    with open(os.path.join(LANCEUR, "outils.json"), encoding="utf-8") as f:
        conf = json.load(f)
    catalogue = conf["outils"]
    ports, procs, lignes, problemes = {}, [], [], 0
    for o in catalogue:
        ports[o["id"]] = (os.path.join(DOCUMENTS, o["dossiers"][0]), o["port"])
    suite = next(o for o in catalogue if o["id"] == "web_suite")
    for id_, port in suite["relais_sous_outils"].items():
        ports[id_] = (os.path.join(DOCUMENTS, "web_suite", id_.upper()), port)
    with tempfile.TemporaryDirectory() as tmp:
        serveur.Lanceur.gardien = compte.Gardien(os.path.join(tmp, "c.json"), os.path.join(tmp, "s.json"))
        assert serveur.Lanceur.gardien.ajouter("verif", "un-mot-de-passe-long") is None
        jeton, _ = serveur.Lanceur.gardien.verifier("verif", "un-mot-de-passe-long", "127.0.0.1")
        serveur.Lanceur.infos = {"port": 0, "dossier_outils": DOCUMENTS}
        relayes = {}
        serveur.Lanceur.gestionnaire = types.SimpleNamespace(par_id={}, port_sous_outil=relayes.get)
        httpd = serveur.ThreadingHTTPServer(("127.0.0.1", 0), serveur.Relais)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        port_relais = httpd.server_address[1]
        try:
            for id_, (dossier, port) in ports.items():
                erreur = code_hote(dossier, nom) if os.path.isdir(dossier) else "dossier absent"
                etat = hote_vivant(port, nom)
                port_essai = port
                if id_ in SCRIPTS:
                    p, port_essai = demarrer(dossier, SCRIPTS[id_])
                    procs.append(p)
                    etat += ", copie neuve"
                    if not reseau.port_ouvert(port_essai):
                        erreur = erreur or "ne démarre pas"
                    elif statut(port_essai, Host="%s:%d" % (nom, port_essai))[0] == 403:
                        erreur = erreur or "nom Tailscale refusé"
                relayes[id_] = port_essai
                if etat.startswith("arrêté") and id_ not in SCRIPTS:
                    relais_txt = "relais non essayé"
                else:
                    r = None if erreur == "ne démarre pas" else par_relais(port_relais, jeton, id_)
                    relais_txt = "relais OK" if r is None else "relais : " + r
                    erreur = erreur or r
                mauvais = erreur or etat.startswith("PIÈGE")
                problemes += bool(mauvais)
                lignes.append("%s %-14s %s, %s%s" % ("KO" if mauvais else "ok", id_, etat, relais_txt,
                                                     " — " + erreur if erreur else ""))
            launcher = hote_vivant(conf["port"], nom)
            if launcher == "PIÈGE ACCEPTÉ":
                problemes += 1
            lignes.append("%s %-14s %s" % ("KO" if launcher.startswith("PIÈGE") else "ok", "web_launcher", launcher))
        finally:
            for p in procs:
                p.terminate()
            for p in procs:
                try:
                    p.wait(10)
                except subprocess.TimeoutExpired:
                    p.kill()
            httpd.shutdown()
            httpd.server_close()
    print("\n".join(lignes))
    print("PROBLÈMES : %d" % problemes if problemes else "OK : %d outils" % len(lignes))
    return 1 if problemes else 0


if __name__ == "__main__":
    sys.exit(main())
