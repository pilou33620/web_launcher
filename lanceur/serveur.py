"""Serveur web du launcher : la page (web/) et l'API JSON utilisée par la page."""

import ipaddress
import json
import os
import re
import sys
import threading
import time
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from . import config, depot, reseau
from .gestionnaire import OccupeErreur

DOSSIER_WEB = os.path.join(config.RACINE, "web")


class Lanceur(SimpleHTTPRequestHandler):
    gestionnaire = None
    infos = {}                      # adresses, nom mDNS... (rempli par main)
    alias = None
    relancer = None                 # fonction : redémarre le launcher (après sa mise à jour)

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=DOSSIER_WEB, **kwargs)

    def log_message(self, *args):   # pas une ligne par requête dans le journal
        pass

    # -- sécurité -------------------------------------------------------

    def _hote_permis(self):
        """Parade au DNS rebinding : le nom demandé doit désigner ce Raspberry Pi."""
        h = (self.headers.get("Host") or "").strip().lower().rstrip(".")
        if h.startswith("["):
            h = h[1:].split("]")[0]
        elif h.count(":") == 1:
            h = h.split(":")[0]
        nom = reseau.nom_machine()
        alias = (self.infos.get("nom_mdns") or "").lower()
        if h in ("", "localhost", nom, nom + ".local", alias, alias + ".local"):
            return True
        try:
            ipaddress.ip_address(h.split("%")[0])
            return True
        except ValueError:
            return False

    def parse_request(self):
        if not super().parse_request():
            return False
        if not self._hote_permis():
            self.send_error(403, "Nom d'hôte non autorisé")
            return False
        return True

    def end_headers(self):
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Content-Type-Options", "nosniff")
        super().end_headers()

    # -- réponses -------------------------------------------------------

    def _json(self, donnees, statut=200):
        corps = json.dumps(donnees, ensure_ascii=False).encode("utf-8")
        self.send_response(statut)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(corps)))
        self.end_headers()
        self.wfile.write(corps)

    def _texte(self, texte, statut=200):
        corps = texte.encode("utf-8")
        self.send_response(statut)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(corps)))
        self.end_headers()
        self.wfile.write(corps)

    def etat(self):
        g = self.gestionnaire
        infos = dict(self.infos)
        infos["ip"] = reseau.ip_locale()
        port = infos["port"]
        suffixe = "" if port == 80 else ":%d" % port
        adresses = []
        if infos["ip"]:
            adresses.append({"type": "Réseau", "url": "http://%s%s/" % (infos["ip"], suffixe)})
        if infos.get("nom_mdns") and self.alias and self.alias.etat in ("annoncé", "nom de la machine"):
            adresses.append({"type": "Nom", "url": "http://%s.local%s/" % (infos["nom_mdns"], suffixe)})
        adresses.append({"type": "Machine", "url": "http://%s.local%s/" % (reseau.nom_machine(), suffixe)})
        infos.update(adresses=adresses, mdns=self.alias.etat if self.alias else "désactivé",
                     version=depot.version(config.RACINE), maj=g.launcher_maj,
                     action=g.launcher_action, heure=int(time.time()))
        return {"launcher": infos, "outils": g.etat()}

    # -- routes ---------------------------------------------------------

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/api/etat":
            return self._json(self.etat())
        if url.path.startswith("/api/journal/"):
            o = self.gestionnaire.par_id.get(url.path.rsplit("/", 1)[1])
            if o is None:
                return self._json({"erreur": "outil inconnu"}, 404)
            lignes = int(parse_qs(url.query).get("lignes", ["300"])[0])
            return self._texte(lire_fin(o.journal, lignes))
        if url.path.startswith("/api/"):
            return self._json({"erreur": "route inconnue"}, 404)
        if url.path not in ("/", "/index.html") and not url.path.startswith(("/css/", "/js/", "/icones/")):
            return self.send_error(404)
        return super().do_GET()

    def do_POST(self):
        # En-tête propre au launcher : un autre site ne peut pas l'envoyer sans
        # autorisation CORS (que le launcher ne donne jamais).
        if self.headers.get("X-Launcher") != "1":
            return self._json({"erreur": "requête refusée"}, 403)
        longueur = int(self.headers.get("Content-Length") or 0)
        if longueur:
            self.rfile.read(min(longueur, 4096))
        morceaux = urlparse(self.path).path.strip("/").split("/")
        g = self.gestionnaire
        try:
            if morceaux[:2] == ["api", "outils"] and len(morceaux) == 4:
                g.lancer_action(morceaux[2], morceaux[3])
                return self._json({"ok": True})
            if morceaux == ["api", "verifier_tout"]:
                for o in g.outils:
                    if o.installe:
                        try:
                            g.lancer_action(o.id, "verifier")
                        except OccupeErreur:
                            pass
                threading.Thread(target=g.verifier_launcher, daemon=True).start()
                return self._json({"ok": True})
            if morceaux == ["api", "launcher", "verifier"]:
                return self._json({"ok": True, "maj": g.verifier_launcher()})
            if morceaux == ["api", "launcher", "mettre_a_jour"]:
                return self._maj_launcher()
        except KeyError:
            return self._json({"erreur": "outil ou action inconnu"}, 404)
        except OccupeErreur as exc:
            return self._json({"erreur": str(exc)}, 409)
        return self._json({"erreur": "route inconnue"}, 404)

    def _maj_launcher(self):
        g = self.gestionnaire
        if g.launcher_action:
            return self._json({"erreur": "déjà en cours"}, 409)
        g.launcher_action = "Mise à jour"
        try:
            relancer = g.mettre_a_jour_launcher()
        except depot.ErreurGit as exc:
            g.launcher_action = None
            return self._json({"erreur": str(exc)}, 500)
        if not relancer:
            g.launcher_action = None
            return self._json({"ok": True, "message": "Déjà à jour."})
        g.launcher_action = "Redémarrage"
        threading.Timer(0.8, self.relancer).start()
        return self._json({"ok": True, "message": "Mis à jour, redémarrage du launcher…"})


def lire_fin(chemin, lignes):
    try:
        with open(chemin, "rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - 200 * 1024))
            texte = f.read().decode("utf-8", "replace")
    except OSError:
        return "(journal vide)"
    texte = "\n".join(texte.splitlines()[-max(1, min(lignes, 3000)):])
    # le jeton réseau de WEB·SUITE ouvre l'accès sans code d'appairage : jamais affiché
    return re.sub(r"jeton=[\w-]+", "jeton=•••", texte)


def ouvrir(port, port_secours, ecrire=print):
    """Ouvre le port voulu (80 pour « http://launcher.local » sans numéro), sinon le port de secours."""
    for p in (port, port_secours):
        if p is None:
            continue
        try:
            return ThreadingHTTPServer(("0.0.0.0", p), Lanceur)
        except PermissionError:
            ecrire("  Port %d refusé (il faut le service systemd du launcher, voir README) ; essai du port %s."
                   % (p, port_secours))
        except OSError as exc:
            ecrire("  Port %d indisponible (%s)." % (p, exc))
    raise SystemExit("Impossible d'ouvrir un port pour le launcher.")


def relancer_processus():
    """Remplace le processus par la nouvelle version (même PID : systemd et les outils ne voient rien)."""
    sys.stdout.flush()
    os.execv(sys.executable, [sys.executable, os.path.abspath(sys.argv[0])] + sys.argv[1:])
